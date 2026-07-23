//! Minimal example: window + TypstView rendering math.
//!
//! Run with: cargo run --example render-math

use floem::prelude::*;
use floem_typst::TypstView;

fn main() {
    floem::launch(app_view);
}

fn app_view() -> impl IntoView {
    let typst_view = TypstView::new();

    // Push some Typst content with math.
    typst_view.push("Hello from Typst! ");
    typst_view.push("Here is some math: $x^2 + y^2 = z^2$ ");
    typst_view.push("and a display equation:\n\n");
    typst_view.push("$ integral_0^infinity e^(-x^2) dif x = sqrt(pi) / 2 $\n\n");
    typst_view.push("And a matrix: $mat(1, 2; 3, 4)$");
    typst_view.flush();

    typst_view.style(|s| {
        s.size(600.0, 400.0)
            .padding(16.0)
            .background(Color::from_rgb8(255, 255, 255))
    })
}
