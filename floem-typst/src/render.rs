//! Frame → floem paint commands.
//!
//! Walks a Typst `Frame` tree and produces floem-compatible draw operations.

use floem::context::PaintCx;
use floem::kurbo::{BezPath, Point as KurboPoint, Rect as KurboRect};
use floem::peniko::Color;
use floem::text::{Attrs, AttrsList, FamilyOwned, Style, TextLayout, Weight};
use floem::Renderer;

use typst_library::layout::{Frame, FrameItem, GroupItem};
use typst_library::text::TextItem;
use typst_library::visualize::{Curve, CurveItem, Geometry, Paint, Shape};

/// Line height (in points) for raw-text fallback drawing. Must match
/// `stream::FALLBACK_LINE_HEIGHT`.
pub const FALLBACK_LINE_HEIGHT: f64 = 18.0;

/// Draw raw source text line-by-line (no wrapping) as a fallback when a
/// block fails to compile. Returns the total height drawn so the caller's
/// height model stays consistent. Used for markdown constructs that are not
/// valid Typst (e.g. `#` headings) so the panel never shows blank.
pub fn paint_fallback_text(
    cx: &mut PaintCx,
    text: &str,
    x: f64,
    y: f64,
    color: Color,
) -> f64 {
    if text.is_empty() {
        return 0.0;
    }
    let attrs = Attrs::new().font_size(13.0).color(color);
    for (i, line) in text.split('\n').enumerate() {
        let content = if line.is_empty() { " " } else { line };
        let layout =
            TextLayout::new_with_text(content, AttrsList::new(attrs.clone()), None);
        cx.draw_text(&layout, KurboPoint::new(x, y + i as f64 * FALLBACK_LINE_HEIGHT));
    }
    (text.matches('\n').count() + 1) as f64 * FALLBACK_LINE_HEIGHT
}

/// Render a Typst Frame into a floem PaintCx at the given offset.
pub fn paint_frame(cx: &mut PaintCx, frame: &Frame, x: f64, y: f64) {
    for (pos, item) in frame.items() {
        let px = x + pos.x.to_pt();
        let py = y + pos.y.to_pt();

        match item {
            FrameItem::Group(group) => paint_group(cx, group, px, py),
            FrameItem::Text(text) => paint_text(cx, text, px, py),
            FrameItem::Shape(shape, _span) => paint_shape(cx, shape, px, py),
            FrameItem::Image(_image, size, _span) => {
                // TODO: Decode image data, create floem image texture, draw
                let w = size.x.to_pt();
                let h = size.y.to_pt();
                let rect = KurboRect::new(px, py, px + w, py + h);
                let mut path = BezPath::new();
                path.move_to(KurboPoint::new(rect.x0, rect.y0));
                path.line_to(KurboPoint::new(rect.x1, rect.y0));
                path.line_to(KurboPoint::new(rect.x1, rect.y1));
                path.line_to(KurboPoint::new(rect.x0, rect.y1));
                path.close_path();
                cx.fill(&path, Color::from_rgb8(200, 200, 200), 0.0);
            }
            FrameItem::Link(_dest, _size) => {
                // TODO: Register hit-test region for clickable links
            }
            FrameItem::Tag(_tag) => {
                // Introspectable metadata — no visual output
            }
        }
    }
}

/// Render a group (subframe with transform and optional clipping).
fn paint_group(cx: &mut PaintCx, group: &GroupItem, x: f64, y: f64) {
    // TODO: Apply full affine transform and clip path.
    // For now, just offset by the transform's translation.
    let t = &group.transform;
    let tx = x + t.tx.to_pt();
    let ty = y + t.ty.to_pt();
    paint_frame(cx, &group.frame, tx, ty);
}

/// Render a text run (positioned glyphs from Typst's shaper).
fn paint_text(cx: &mut PaintCx, text: &TextItem, x: f64, y: f64) {
    if text.glyphs.is_empty() {
        return;
    }

    let font = &text.font;
    let font_size = text.size.to_pt() as f32;

    // Reconstruct the text content from the glyph ranges.
    let content: String = text
        .glyphs
        .iter()
        .filter_map(|g| {
            let range = g.range();
            text.text.get(range).map(|s| s.to_string())
        })
        .collect();

    if content.is_empty() {
        return;
    }

    // Build attrs from the Typst font info.
    let info = font.font().info();
    let family: Vec<FamilyOwned> = FamilyOwned::parse_list(&info.family).collect();

    let weight = Weight(info.variant.weight.to_number());
    let font_style = match info.variant.style {
        typst_library::text::FontStyle::Italic => Style::Italic,
        typst_library::text::FontStyle::Oblique => Style::Oblique,
        _ => Style::Normal,
    };

    let color = typst_paint_to_floem(&text.fill);

    let attrs = Attrs::new()
        .family(&family)
        .font_size(font_size)
        .weight(weight)
        .style(font_style)
        .color(color);

    let text_layout = TextLayout::new_with_text(&content, AttrsList::new(attrs), None);

    // Typst positions text at the baseline; floem draws from the top.
    // Approximate ascent as ~80% of font size.
    let draw_y = y - font_size as f64 * 0.8;

    cx.draw_text(&text_layout, KurboPoint::new(x, draw_y));
}

/// Render a shape (rect, curve, line) with fill and/or stroke.
fn paint_shape(cx: &mut PaintCx, shape: &Shape, x: f64, y: f64) {
    let path = geometry_to_bezpath(&shape.geometry, x, y);

    if let Some(fill) = &shape.fill {
        let color = typst_paint_to_floem(fill);
        cx.fill(&path, color, 0.0);
    }

    if let Some(stroke) = &shape.stroke {
        let color = typst_paint_to_floem(&stroke.paint);
        let width = stroke.thickness.to_pt();
        let stroke_style = floem::kurbo::Stroke::new(width);
        cx.stroke(&path, color, &stroke_style);
    }
}

// --- Conversion helpers ---

fn geometry_to_bezpath(geometry: &Geometry, x: f64, y: f64) -> BezPath {
    let mut path = BezPath::new();

    match geometry {
        Geometry::Line(target) => {
            path.move_to(KurboPoint::new(x, y));
            path.line_to(KurboPoint::new(x + target.x.to_pt(), y + target.y.to_pt()));
        }
        Geometry::Rect(size) => {
            let w = size.x.to_pt();
            let h = size.y.to_pt();
            path.move_to(KurboPoint::new(x, y));
            path.line_to(KurboPoint::new(x + w, y));
            path.line_to(KurboPoint::new(x + w, y + h));
            path.line_to(KurboPoint::new(x, y + h));
            path.close_path();
        }
        Geometry::Curve(curve) => {
            convert_curve(curve, x, y, &mut path);
        }
    }

    path
}

fn convert_curve(curve: &Curve, x: f64, y: f64, bez: &mut BezPath) {
    for item in &curve.0 {
        match item {
            CurveItem::Move(p) => {
                bez.move_to(KurboPoint::new(x + p.x.to_pt(), y + p.y.to_pt()));
            }
            CurveItem::Line(p) => {
                bez.line_to(KurboPoint::new(x + p.x.to_pt(), y + p.y.to_pt()));
            }
            CurveItem::Cubic(p1, p2, p3) => {
                bez.curve_to(
                    KurboPoint::new(x + p1.x.to_pt(), y + p1.y.to_pt()),
                    KurboPoint::new(x + p2.x.to_pt(), y + p2.y.to_pt()),
                    KurboPoint::new(x + p3.x.to_pt(), y + p3.y.to_pt()),
                );
            }
            CurveItem::Close => {
                bez.close_path();
            }
        }
    }
}

fn typst_paint_to_floem(paint: &Paint) -> Color {
    match paint {
        Paint::Solid(color) => typst_color_to_floem(color),
        Paint::Gradient(_) => Color::from_rgb8(128, 128, 128),
        Paint::Tiling(_) => Color::from_rgb8(128, 128, 128),
    }
}

fn typst_color_to_floem(color: &typst_library::visualize::Color) -> Color {
    let rgb = color.to_rgb();
    let r = (rgb.red.clamp(0.0, 1.0) * 255.0) as u8;
    let g = (rgb.green.clamp(0.0, 1.0) * 255.0) as u8;
    let b = (rgb.blue.clamp(0.0, 1.0) * 255.0) as u8;
    let a = (rgb.alpha.clamp(0.0, 1.0) * 255.0) as u8;
    Color::from_rgba8(r, g, b, a)
}
