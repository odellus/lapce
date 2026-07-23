//! The floem View that renders streaming Typst content.

use floem::context::{EventCx, PaintCx};
use floem::event::{Event, EventPropagation};
use floem::peniko::Color;
use floem::reactive::{RwSignal, Scope, SignalGet, SignalUpdate, SignalWith};
use floem::View;
use floem::ViewId;

use crate::render::{paint_fallback_text, paint_frame};
use crate::stream::{Scene, TypstStream};

/// A floem View that renders streaming Typst content.
#[derive(Clone)]
pub struct TypstView {
    id: ViewId,
    stream: RwSignal<TypstStream>,
    scene: RwSignal<Option<Scene>>,
    scroll_y: RwSignal<f64>,
    /// Last compiled total height (in the same pt-as-px units the renderer
    /// uses). Exposed so an outer container can size itself to the content;
    /// a leaf `View` that reports no height lays out to 0px and paints
    /// nothing visible.
    height: RwSignal<f64>,
    /// If true, the view handles its own scroll (wheel events). Set false
    /// when embedded inside an outer scroll container (e.g. the chat panel)
    /// so wheel events bubble to the outer scroller.
    own_scroll: bool,
}

impl TypstView {
    pub fn new() -> Self {
        let id = ViewId::new();
        let stream = RwSignal::new(TypstStream::new());
        let scene = RwSignal::new(None);
        let scroll_y = RwSignal::new(0.0);
        let height = RwSignal::new(0.0);

        Self {
            id,
            stream,
            scene,
            scroll_y,
            height,
            own_scroll: true,
        }
    }

    /// Build a view that defers scrolling to an outer container.
    pub fn new_embedded() -> Self {
        let mut v = Self::new();
        v.own_scroll = false;
        v
    }

    /// Build a view whose reactive state lives on the given (long-lived)
    /// scope. Use this whenever the view is constructed from inside a
    /// callback or `create_effect` closure: bare `RwSignal::new` would bind
    /// the signals to the effect's run-scope, which floem disposes on the
    /// next re-run, leaving dead signals that panic on `.get()`.
    pub fn new_in(scope: Scope) -> Self {
        let id = ViewId::new();
        let stream = scope.create_rw_signal(TypstStream::new());
        let scene = scope.create_rw_signal(None);
        let scroll_y = scope.create_rw_signal(0.0);
        let height = scope.create_rw_signal(0.0);

        Self {
            id,
            stream,
            scene,
            scroll_y,
            height,
            own_scroll: true,
        }
    }

    /// Embedded variant on an explicit scope (see `new_in`).
    pub fn new_embedded_in(scope: Scope) -> Self {
        let mut v = Self::new_in(scope);
        v.own_scroll = false;
        v
    }

    /// Reactive height signal (pt-as-px) for an outer container to bind to.
    pub fn content_height_signal(&self) -> RwSignal<f64> {
        self.height
    }

    /// Push a chunk of Typst source content.
    pub fn push(&self, chunk: &str) {
        self.stream.update(|s| s.push(chunk));
        self.id.request_paint();
    }

    /// Force-flush the current content (e.g., on turn end).
    pub fn flush(&self) {
        let mut new_scene = None;
        self.stream.update(|s| {
            new_scene = Some(s.flush());
        });
        self.sync_height(&new_scene);
        self.scene.set(new_scene);
        self.id.request_paint();
    }

    /// Reset for a new message.
    pub fn reset(&self) {
        self.stream.update(|s| s.reset());
        self.scene.set(None);
        self.scroll_y.set(0.0);
        self.height.set(0.0);
        self.id.request_paint();
    }

    /// Get the total content height.
    pub fn content_height(&self) -> f64 {
        self.scene
            .with(|s| s.as_ref().map_or(0.0, |scene| scene.total_height))
    }

    /// Publish the compiled height so an outer container can size itself.
    fn sync_height(&self, scene: &Option<Scene>) {
        let h = scene.as_ref().map_or(0.0, |s| s.total_height);
        if (self.height.get_untracked() - h).abs() > 0.5 {
            self.height.set(h);
        }
    }

    /// Tick the streaming compiler if debounce interval has elapsed.
    fn maybe_tick(&self) {
        let should = self.stream.with(|s| s.should_tick());
        if should {
            let mut new_scene = None;
            let mut changed = false;
            self.stream.update(|s| {
                let scene = s.tick();
                changed = scene.changed;
                new_scene = Some(scene);
            });
            if changed {
                self.sync_height(&new_scene);
                self.scene.set(new_scene);
                self.id.request_paint();
            }
        }
    }
}

impl View for TypstView {
    fn id(&self) -> ViewId {
        self.id
    }

    fn event_before_children(
        &mut self,
        _cx: &mut EventCx,
        event: &Event,
    ) -> EventPropagation {
        if self.own_scroll {
            if let Event::PointerWheel(pwe) = event {
                let size = self.id.layout_rect().size();

                let height = self.content_height();
                let viewport = size.height;
                let max_scroll = (height - viewport).max(0.0);

                let new_scroll = self.scroll_y.get_untracked() + pwe.delta.y;
                let clamped = new_scroll.clamp(0.0, max_scroll);
                self.scroll_y.set(clamped);
                self.id.request_paint();
                return EventPropagation::Stop;
            }
        }
        EventPropagation::Continue
    }

    fn paint(&mut self, cx: &mut PaintCx) {
        self.maybe_tick();
        let scroll = self.scroll_y.get();

        self.scene.with(|scene_opt| {
            let Some(scene) = scene_opt else { return };

            for block in &scene.frozen {
                let y = block.y_offset - scroll;
                paint_frame(cx, &block.frame, 0.0, y);
            }

            if let Some(ref frame) = scene.active.frame {
                let y = scene.active.y_offset - scroll;
                paint_frame(cx, frame, 0.0, y);
            } else if scene.fallback_active {
                let y = scene.active.y_offset - scroll;
                paint_fallback_text(
                    cx,
                    &scene.active.source,
                    8.0,
                    y,
                    Color::from_rgb8(224, 224, 224),
                );
            }
        });
    }
}
