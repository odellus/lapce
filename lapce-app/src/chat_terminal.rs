//! Minimal inline terminal view for ACP tool calls in the chat panel.
//!
//! Renders an alacritty_terminal grid (fed by proxy PTY bytes) inside the
//! chat message list. Read-only — no keyboard input, no selection.

use std::sync::{Arc, RwLock};

use alacritty_terminal::{
    Term,
    event::EventListener,
    grid::{Dimensions, Scroll},
    term::{Config, cell::Flags, test::TermSize},
    vte::ansi,
};
use floem::{
    Renderer, View, ViewId,
    context::{EventCx, PaintCx, UpdateCx},
    event::{Event, EventPropagation},
    kurbo::{Point, Rect, Size},
    reactive::{
        ReadSignal, RwSignal, SignalGet, SignalUpdate, SignalWith, create_effect,
    },
    text::{Attrs, AttrsList, FamilyOwned, TextLayout, Weight},
};
use crate::config::{LapceConfig, color::LapceColor};

/// No-op event listener for read-only chat terminals.
pub struct NoopListener;
impl EventListener for NoopListener {
    fn send_event(&self, _event: alacritty_terminal::event::Event) {}
}

/// The alacritty grid + ANSI parser for a chat inline terminal.
pub struct ChatRawTerminal {
    pub parser: ansi::Processor,
    pub term: Term<NoopListener>,
    /// Accumulated wheel delta (px) not yet consumed into whole-line scrolls,
    /// mirroring the real terminal's `RawTerminal::scroll_delta`.
    pub scroll_delta: f64,
}

impl ChatRawTerminal {
    pub fn new(rows: usize, cols: usize) -> Self {
        let config = Config::default();
        let size = TermSize::new(cols, rows);
        let term = Term::new(config, &size, NoopListener);
        Self {
            parser: ansi::Processor::new(),
            term,
            scroll_delta: 0.0,
        }
    }

    pub fn feed(&mut self, data: &[u8]) {
        for &byte in data {
            self.parser.advance(&mut self.term, byte);
        }
    }
}

/// Handle stored per ACP terminal: the grid plus a generation counter the
/// view tracks so streaming bytes trigger a repaint.
#[derive(Clone)]
pub struct ChatTermHandle {
    pub raw: Arc<RwLock<ChatRawTerminal>>,
    pub paint_gen: RwSignal<u64>,
}

impl ChatTermHandle {
    /// Scroll the grid's display offset in response to a pointer-wheel delta.
    ///
    /// Mirrors the real terminal (`TerminalData::wheel_scroll`): accumulate the
    /// pixel delta and convert to whole lines. Returns `true` when the wheel was
    /// consumed (there is scrollback to move through) so the caller can stop the
    /// event from also scrolling the surrounding chat; `false` when there is no
    /// history and the chat should scroll instead.
    pub fn wheel_scroll(&self, delta_y: f64, line_height: f64) -> bool {
        let Ok(mut raw) = self.raw.write() else {
            return false;
        };
        if raw.term.history_size() == 0 {
            return false;
        }
        raw.scroll_delta -= delta_y;
        let lines = (raw.scroll_delta / line_height) as i32;
        if lines != 0 {
            raw.scroll_delta -= lines as f64 * line_height;
            raw.term.scroll_display(Scroll::Delta(lines));
            self.paint_gen.update(|g| *g += 1);
        }
        true
    }
}

/// Floem View that paints a `ChatRawTerminal` grid.
pub struct ChatTerminalView {
    id: ViewId,
    handle: ChatTermHandle,
    config: ReadSignal<Arc<LapceConfig>>,
    size: Size,
}

pub fn chat_terminal_view(
    handle: ChatTermHandle,
    config: ReadSignal<Arc<LapceConfig>>,
) -> ChatTerminalView {
    let id = ViewId::new();
    // Repaint whenever new bytes are fed (gen bumps). The signal is allocated
    // on the long-lived window-tab scope, so it outlives this view's scope.
    let paint_gen = handle.paint_gen;
    create_effect(move |_| {
        paint_gen.with(|_| {});
        id.request_paint();
    });
    ChatTerminalView {
        id,
        handle,
        config,
        size: Size::ZERO,
    }
}

impl View for ChatTerminalView {
    fn id(&self) -> ViewId {
        self.id
    }

    fn update(&mut self, cx: &mut UpdateCx, _state: Box<dyn std::any::Any>) {
        cx.app_state_mut().request_paint(self.id);
    }

    fn event_before_children(
        &mut self,
        cx: &mut EventCx,
        event: &Event,
    ) -> EventPropagation {
        // Wheel over the terminal scrolls its own scrollback (alacritty /
        // xterm.js behaviour). Only capture the event when there is history to
        // move through; otherwise let the surrounding chat scroll.
        if let Event::PointerWheel(e) = event {
            let config = self.config.get_untracked();
            let line_height = config.terminal_line_height() as f64;
            if self.handle.wheel_scroll(e.delta.y, line_height) {
                cx.app_state_mut().request_paint(self.id);
                return EventPropagation::Stop;
            }
        }
        EventPropagation::Continue
    }

    fn layout(
        &mut self,
        cx: &mut floem::context::LayoutCx,
    ) -> floem::taffy::prelude::NodeId {
        cx.layout_node(self.id, false, |_cx| Vec::new())
    }

    fn compute_layout(
        &mut self,
        _cx: &mut floem::context::ComputeLayoutCx,
    ) -> Option<Rect> {
        let layout = self.id.get_layout().unwrap_or_default();
        let new_size = Size::new(layout.size.width as f64, layout.size.height as f64);
        if new_size != self.size && !new_size.is_zero_area() {
            self.size = new_size;
            let config = self.config.get_untracked();
            let line_height = config.terminal_line_height() as f64;
            let char_width = self.char_width(&config);
            let cols = (new_size.width / char_width).floor().max(1.0) as usize;
            let rows = (new_size.height / line_height).floor().max(1.0) as usize;
            let term_size = TermSize::new(cols, rows);
            if let Ok(mut raw) = self.handle.raw.write() {
                raw.term.resize(term_size);
            }
        }
        None
    }

    fn paint(&mut self, cx: &mut PaintCx) {
        let config = self.config.get_untracked();
        let line_height = config.terminal_line_height() as f64;
        let font_family = config.terminal_font_family();
        let font_size = config.terminal_font_size();
        let family: Vec<FamilyOwned> =
            FamilyOwned::parse_list(font_family).collect();
        let attrs = Attrs::new().family(&family).font_size(font_size as f32);
        let char_width = self.char_width(&config);
        let char_height = self.char_height(&config);

        // Background
        let term_bg = config.color(LapceColor::TERMINAL_BACKGROUND);
        cx.fill(
            &Rect::new(0.0, 0.0, self.size.width, self.size.height),
            term_bg,
            0.0,
        );

        let raw = match self.handle.raw.read() {
            Ok(r) => r,
            Err(_) => return,
        };
        let content = raw.term.renderable_content();

        for item in content.display_iter {
            let point = item.point;
            let cell = item.cell;

            let x = point.column.0 as f64 * char_width;
            // `+ display_offset` pins the scrolled window to the top of the box:
            // display_iter yields lines starting at `-display_offset`, so this
            // normalises the first visible row to y == 0 whatever the offset.
            let y = (point.line.0 as f64 + content.display_offset as f64)
                * line_height;
            let char_y = y + (line_height - char_height) / 2.0;

            let mut bg = config.terminal_get_color(&cell.bg, content.colors);
            let mut fg = config.terminal_get_color(&cell.fg, content.colors);

            if cell.flags.contains(Flags::DIM)
                || cell.flags.contains(Flags::DIM_BOLD)
            {
                fg = fg.multiply_alpha(0.66);
            }

            // INVERSE swaps foreground/background (htop, ls highlights, …).
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }

            if term_bg != bg {
                cx.fill(
                    &Rect::new(x, y, x + char_width, y + line_height),
                    bg,
                    0.0,
                );
            }

            if cell.c != ' ' && cell.c != '\t' {
                let bold = cell.flags.contains(Flags::BOLD)
                    || cell.flags.contains(Flags::DIM_BOLD);
                let mut a = attrs.clone().color(fg);
                if bold {
                    a = a.weight(Weight::BOLD);
                }
                let mut tl = TextLayout::new();
                tl.set_text(&cell.c.to_string(), AttrsList::new(a), None);
                cx.draw_text(&tl, Point::new(x, char_y));
            }

            // Underline / strikeout drawn as 1px rules so they show even on
            // blank cells, matching a real terminal.
            if cell.flags.contains(Flags::UNDERLINE) {
                let uy = (y + char_height + 1.0).min(y + line_height - 1.0);
                cx.fill(&Rect::new(x, uy, x + char_width, uy + 1.0), fg, 0.0);
            }
            if cell.flags.contains(Flags::STRIKEOUT) {
                let sy = y + (line_height / 2.0);
                cx.fill(&Rect::new(x, sy, x + char_width, sy + 1.0), fg, 0.0);
            }
        }

        // Thin scrollbar — only when there is scrollback to communicate.
        let history = raw.term.history_size();
        if history > 0 && self.size.height > 0.0 {
            let screen = raw.term.screen_lines().max(1);
            let offset = raw.term.grid().display_offset().min(history);
            let track_h = self.size.height;
            let thumb_h = (track_h * (screen as f64 / (history + screen) as f64))
                .clamp(16.0, track_h);
            // offset 0 == live bottom; offset == history == fully scrolled up.
            let frac = (history - offset) as f64 / history as f64;
            let thumb_y = (track_h - thumb_h) * frac;

            let gap = 2.0;
            let sb_w = 6.0;
            let x0 = self.size.width - sb_w - gap;
            let x1 = self.size.width - gap;
            let thumb = config.color(LapceColor::LAPCE_SCROLL_BAR);
            let track = thumb.multiply_alpha(0.18);
            cx.fill(&Rect::new(x0, 0.0, x1, track_h), track, 3.0);
            cx.fill(
                &Rect::new(x0, thumb_y, x1, thumb_y + thumb_h),
                thumb,
                3.0,
            );
        }
    }
}

impl ChatTerminalView {
    fn char_width(&self, config: &LapceConfig) -> f64 {
        let family: Vec<FamilyOwned> =
            FamilyOwned::parse_list(config.terminal_font_family()).collect();
        let attrs = Attrs::new()
            .family(&family)
            .font_size(config.terminal_font_size() as f32);
        let mut tl = TextLayout::new();
        tl.set_text("W", AttrsList::new(attrs), None);
        tl.size().width
    }

    fn char_height(&self, config: &LapceConfig) -> f64 {
        let family: Vec<FamilyOwned> =
            FamilyOwned::parse_list(config.terminal_font_family()).collect();
        let attrs = Attrs::new()
            .family(&family)
            .font_size(config.terminal_font_size() as f32);
        let mut tl = TextLayout::new();
        tl.set_text("W", AttrsList::new(attrs), None);
        tl.size().height
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_raw_terminal_feed_populates_grid() {
        let mut term = ChatRawTerminal::new(24, 80);
        // Real PTY output uses \r\n (carriage return + line feed).
        // Bare \n moves down but doesn't reset column.
        term.feed(b"hello world\r\n");
        term.feed(b"line two\r\n");

        let content = term.term.renderable_content();
        let mut line0_text = String::new();
        let mut line1_text = String::new();
        for item in content.display_iter {
            let c = item.cell.c;
            match item.point.line.0 {
                0 => line0_text.push(c),
                1 => line1_text.push(c),
                _ => {}
            }
        }
        assert!(
            line0_text.starts_with("hello world"),
            "line 0: expected 'hello world', got: {line0_text:?}"
        );
        assert!(
            line1_text.starts_with("line two"),
            "line 1: expected 'line two', got: {line1_text:?}"
        );
    }

    #[test]
    fn chat_raw_terminal_feed_ansi_colors() {
        let mut term = ChatRawTerminal::new(24, 80);
        term.feed(b"\x1b[31mRED\x1b[0m normal\n");

        let content = term.term.renderable_content();
        let mut chars = Vec::new();
        for item in content.display_iter {
            if item.point.line.0 == 0 {
                chars.push((
                    item.cell.c,
                    item.cell.fg,
                ));
            }
        }
        // First 3 chars should be R, E, D
        assert_eq!(chars[0].0, 'R');
        assert_eq!(chars[1].0, 'E');
        assert_eq!(chars[2].0, 'D');
        // R should have a non-default fg (red = indexed color 1)
        // The exact representation depends on the alacritty version, but
        // it should NOT be the default foreground.
        assert_ne!(
            chars[0].1, chars[4].1,
            "RED chars should have different fg than 'normal' chars"
        );
    }

    #[test]
    fn chat_raw_terminal_empty_feed_no_crash() {
        let mut term = ChatRawTerminal::new(24, 80);
        term.feed(b"");
        let content = term.term.renderable_content();
        let count = content.display_iter.count();
        // Empty terminal still has 24*80 cells in display_iter
        assert_eq!(count, 24 * 80);
    }

    #[test]
    fn chat_raw_terminal_large_output_scrolls() {
        let mut term = ChatRawTerminal::new(24, 80);
        for i in 0..100 {
            // Real PTY output uses \r\n; bare \n would stagger columns off
            // the right edge and never scroll cleanly.
            term.feed(format!("line {i}\r\n").as_bytes());
        }
        // After 100 lines in a 24-row terminal, the visible region should
        // show the LAST ~24 lines (76-99). Read via display_iter (top-to-
        // bottom), which is exactly what paint() uses.
        let content = term.term.renderable_content();
        let mut visible_text = String::new();
        for item in content.display_iter {
            if item.cell.c != ' ' && item.cell.c != '\0' {
                visible_text.push(item.cell.c);
            }
        }
        // Should contain "line99" (last line written; spaces stripped).
        assert!(
            visible_text.contains("line99"),
            "expected 'line99' in visible output after scrolling, got: {visible_text:?}"
        );
        // line0 should have scrolled off the top of the visible region.
        assert!(
            !visible_text.contains("line0"),
            "line0 should have scrolled off the visible region, got: {visible_text:?}"
        );
    }

    #[test]
    fn chat_raw_terminal_resize_preserves_content() {
        let mut term = ChatRawTerminal::new(24, 80);
        term.feed(b"hello\n");

        // Resize to 12 rows, 40 cols
        let new_size = alacritty_terminal::term::test::TermSize::new(40, 12);
        term.term.resize(new_size);

        let content = term.term.renderable_content();
        let mut text = String::new();
        for item in content.display_iter {
            if item.point.line.0 == 0 && item.cell.c != ' ' && item.cell.c != '\0' {
                text.push(item.cell.c);
            }
        }
        assert!(
            text.starts_with("hello"),
            "content should survive resize, got: {text:?}"
        );
    }

    #[test]
    fn chat_terminal_wheel_scrolls_history() {
        use floem::reactive::Scope;
        let scope = Scope::new();
        let raw = Arc::new(RwLock::new(ChatRawTerminal::new(24, 80)));
        // Fill well past the screen so there is scrollback.
        for i in 0..100 {
            raw.write().unwrap().feed(format!("line {i}\r\n").as_bytes());
        }
        let handle = ChatTermHandle {
            raw: raw.clone(),
            paint_gen: scope.create_rw_signal(0u64),
        };

        // Live bottom before any wheel input.
        assert_eq!(raw.read().unwrap().term.grid().display_offset(), 0);
        assert!(raw.read().unwrap().term.history_size() > 0);

        let line_height = 18.0;
        // Wheel "up" (negative delta_y) scrolls up into history.
        let consumed = handle.wheel_scroll(-line_height * 5.0, line_height);
        assert!(consumed, "wheel over scrollback should be consumed");
        let offset_after = raw.read().unwrap().term.grid().display_offset();
        assert!(
            offset_after > 0,
            "display_offset should increase after scrolling up, got {offset_after}"
        );

        // Scrolling far back down clamps to the live bottom.
        let _ = handle.wheel_scroll(line_height * 100.0, line_height);
        assert_eq!(
            raw.read().unwrap().term.grid().display_offset(),
            0,
            "scrolling far down should clamp back to the live bottom"
        );
    }

    #[test]
    fn chat_terminal_wheel_no_history_not_consumed() {
        use floem::reactive::Scope;
        let scope = Scope::new();
        let raw = Arc::new(RwLock::new(ChatRawTerminal::new(24, 80)));
        raw.write().unwrap().feed(b"just one line\r\n");
        let handle = ChatTermHandle {
            raw,
            paint_gen: scope.create_rw_signal(0u64),
        };
        // No scrollback → wheel is not consumed (the chat scrolls instead).
        assert!(!handle.wheel_scroll(-100.0, 18.0));
    }
}
