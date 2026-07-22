//! Minimal inline terminal view for ACP tool calls in the chat panel.
//!
//! Renders an alacritty_terminal grid (fed by proxy PTY bytes) inside the
//! chat message list. Read-only — no keyboard input, no selection.

use std::sync::{Arc, RwLock};

use alacritty_terminal::{
    Term,
    event::EventListener,
    term::{Config, test::TermSize},
    vte::ansi,
};
use floem::{
    Renderer, View, ViewId,
    context::{PaintCx, UpdateCx},
    kurbo::{Point, Rect, Size},
    reactive::{ReadSignal, RwSignal, SignalGet, SignalWith, create_effect},
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
}

impl ChatRawTerminal {
    pub fn new(rows: usize, cols: usize) -> Self {
        let config = Config::default();
        let size = TermSize::new(cols, rows);
        let term = Term::new(config, &size, NoopListener);
        Self {
            parser: ansi::Processor::new(),
            term,
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

/// Floem View that paints a `ChatRawTerminal` grid.
pub struct ChatTerminalView {
    id: ViewId,
    raw: Arc<RwLock<ChatRawTerminal>>,
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
        raw: handle.raw,
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
            if let Ok(mut raw) = self.raw.write() {
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

        // Background
        let bg = config.color(LapceColor::TERMINAL_BACKGROUND);
        cx.fill(
            &Rect::new(0.0, 0.0, self.size.width, self.size.height),
            bg,
            0.0,
        );

        let raw = match self.raw.read() {
            Ok(r) => r,
            Err(_) => return,
        };
        let content = raw.term.renderable_content();

        for item in content.display_iter {
            let point = item.point;
            let cell = item.cell;

            let x = point.column.0 as f64 * char_width;
            let y = (point.line.0 as f64 + content.display_offset as f64)
                * line_height;
            let char_y = y + (line_height - self.char_height(&config)) / 2.0;

            let cell_bg = config.terminal_get_color(&cell.bg, content.colors);
            let mut fg = config.terminal_get_color(&cell.fg, content.colors);

            if cell.flags.contains(alacritty_terminal::term::cell::Flags::DIM) {
                fg = fg.multiply_alpha(0.66);
            }

            if bg != cell_bg {
                cx.fill(
                    &Rect::new(x, y, x + char_width, y + line_height),
                    cell_bg,
                    0.0,
                );
            }

            if cell.c != ' ' && cell.c != '\t' {
                let bold = cell.flags
                    .contains(alacritty_terminal::term::cell::Flags::BOLD);
                let mut a = attrs.clone().color(fg);
                if bold {
                    a = a.weight(Weight::BOLD);
                }
                let mut tl = TextLayout::new();
                tl.set_text(&cell.c.to_string(), AttrsList::new(a), None);
                cx.draw_text(&tl, Point::new(x, char_y));
            }
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
}
