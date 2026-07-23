# Context for floem-typst compilation fix

## Key API corrections (from analysis of actual typst 0.15.1 + floem source)

### floem Renderer trait (PaintCx derefs to this):
```rust
fn fill(&mut self, path: &impl Shape, brush: impl Into<BrushRef>, blur_radius: f64);
fn stroke(&mut self, shape: &impl Shape, brush: impl Into<BrushRef>, stroke: &Stroke);
fn draw_text(&mut self, layout: &TextLayout, pos: impl Into<Point>);
fn draw_img(&mut self, img: Img, rect: Rect);
fn draw_svg(&mut self, svg: Svg, rect: Rect, brush: Option<impl Into<BrushRef>>);
fn push_layer(&mut self, blend: impl Into<BlendMode>, alpha: f32, transform: Affine, clip: &impl Shape);
fn pop_layer(&mut self);
```

### typst-library types (v0.15.1):
- `Frame.items()` → `&[(Point, FrameItem)]`
- `Frame.size()` → `Size` (has `.x: Abs`, `.y: Abs`, call `.to_pt()` for f64)
- `TextItem { font: FontInstance, size: Abs, fill: Paint, text: EcoString, glyphs: Vec<Glyph> }`
- `Glyph { id: u16, x_advance: Em, x_offset: Em, y_advance: Em, y_offset: Em, range: Range<u16> }`
- **CRITICAL: Y-up coordinates.** SVG renderer flips with scale(1,-1). We need the same.
- `Geometry` enum is: `Line(Point) | Rect(Size) | Curve(Curve)` — NOT `Path`
- `CurveItem`: `Move(Point) | Line(Point) | Cubic(Point, Point, Point) | Close`
- `GroupItem { frame: Frame, transform: Transform, clip: Option<Curve>, label, parent }`
- `Transform { sx: Ratio, ky: Ratio, kx: Ratio, sy: Ratio, tx: Abs, ty: Abs }`
- `Paint`: `Solid(Color) | Gradient(Gradient) | Tiling(Tiling)`
- `Color`: `Luma(u8) | Rgb(u8,u8,u8) | Rgba(u8,u8,u8,u8) | Cmyk(u8,u8,u8,u8) | Hsv(...)`
- `Abs.to_pt()` → f64

### typst::compile():
- Takes `&dyn World` (or `&W where W: World`)
- Returns `Warned<SourceResult<Document>>`
- `Document { pages: Vec<Page> }`, `Page { frame: Frame, fill: Paint, numbering: ... }`

### World trait (must implement):
- `root() -> PathBuf`
- `library() -> Arc<LazyHash<Library>>`
- `book() -> &LazyHash<FontBook>`
- `main() -> FileId`
- `source(id: FileId) -> FileResult<Source>`
- `file(id: FileId) -> FileResult<Bytes>`
- `font(index: usize) -> Option<Font>`
- `today(offset: Option<i64>) -> Option<Datetime>`

### floem View trait:
- Only `fn id(&self) -> ViewId` is required
- `fn paint(&mut self, cx: &mut PaintCx)` — the drawing hook
- `fn layout(&mut self, cx: &mut LayoutCx) -> NodeId` — uses taffy
- `fn event_before_children(&mut self, cx: &mut EventCx, event: &Event) -> EventPropagation`
- ViewId::new() creates a view id
- id.create_rw_signal(val) for reactive state
- id.request_paint() to trigger repaint

### What to fix in floem-typst/src/render.rs:
1. `cx.fill(&mut path, color, None)` → `cx.fill(&path, color, 0.0)` (blur_radius is f64, not Option)
2. `cx.stroke(&stroke_style, &mut path, color, None)` → `cx.stroke(&path, color, &stroke_style)`
3. `Geometry::Path(typst_path)` → `Geometry::Curve(curve)` with `CurveItem` variants
4. Add Y-flip for text rendering
5. Use `text.text` (EcoString) directly instead of reconstructing from glyphs
6. Fix `FontInstance` → get family name (check how to access inner Font)
7. Fix Color enum variants to match actual typst 0.15.1

### What to fix in floem-typst/src/view.rs:
1. `layout()` should return `NodeId` not `Size` — use taffy layout system
2. Check how other lapce views do layout (they use cx.layout_node or similar)

### What to fix in floem-typst/src/world.rs:
1. Check exact World trait methods for 0.15.1
2. `Font::iter(Bytes)` — verify this exists
3. `LazyHash` import path

## Files to modify:
- ~/src/crow-team/floem-typst/src/render.rs
- ~/src/crow-team/floem-typst/src/view.rs
- ~/src/crow-team/floem-typst/src/world.rs
- ~/src/crow-team/floem-typst/src/stream.rs
- ~/src/crow-team/floem-typst/Cargo.toml

## Goal:
Get `cargo build` passing. Then write a minimal example (src/main.rs or examples/basic.rs) that:
1. Opens a floem window
2. Creates a TypstView
3. Pushes "Hello $x^2$ world"
4. Renders it

Reference the actual typst source at ~/src/crow-team/typst and floem source in cargo git checkouts.
