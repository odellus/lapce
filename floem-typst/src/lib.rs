//! floem-typst: Streaming Typst rendering as a floem View.
//!
//! This crate provides a floem `View` that renders Typst markup with
//! incremental/streaming compilation. Designed for chat message rendering
//! where Typst source arrives token-by-token.
//!
//! # Architecture
//!
//! ```text
//! Typst source (streamed)
//!   → typst::compile() [debounced, ~80ms]
//!   → Document { pages: [Page { frame: Frame }] }
//!   → Frame { items: Vec<(Point, FrameItem)> }
//!   → floem paint commands (TextLayout, fill, stroke, image)
//! ```
//!
//! # Streaming Strategy
//!
//! Completed blocks are compiled once and frozen (positioned glyphs cached).
//! Only the "active tail" (current incomplete block) is recompiled per tick.
//! This gives O(active_tail) cost per update, not O(document).

pub mod render;
pub mod stream;
pub mod view;
pub mod world;

pub use view::TypstView;
pub use stream::TypstStream;

use typst::diag::Warned;
use typst_layout::PagedDocument;
use typst_library::foundations::Smart;
use typst_pdf::{PdfOptions, pdf};

use world::StreamWorld;

/// Compile Typst source and export to PDF bytes.
///
/// Returns the PDF as a `Vec<u8>`, or an error string if compilation fails.
pub fn render_to_pdf(source: &str) -> Result<Vec<u8>, String> {
    let mut world = StreamWorld::new();
    world.set_source(source);

    let Warned { output, warnings } = typst::compile::<PagedDocument>(&world);
    for w in &warnings {
        tracing::warn!("Typst warning: {:?}", w);
    }

    let doc = output.map_err(|errs| {
        errs.iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
            .join("\n")
    })?;

    let options = PdfOptions {
        ident: Smart::Auto,
        ..Default::default()
    };

    pdf(&doc, &options).map_err(|errs| {
        errs.iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
            .join("\n")
    })
}
