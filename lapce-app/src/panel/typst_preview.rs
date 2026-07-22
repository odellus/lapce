use std::rc::Rc;

use floem::{
    View,
    views::{Decorators, container, label, stack},
};

use super::position::PanelPosition;
use crate::window_tab::WindowTabData;

pub fn typst_preview_panel(
    window_tab_data: Rc<WindowTabData>,
    _position: PanelPosition,
) -> impl View {
    let typst_view = window_tab_data.typst_view.clone();

    container(
        stack((
            // Toolbar
            container(
                label(|| "Typst Preview".to_string()).style(|s| {
                    s.font_bold().padding(4.0)
                }),
            )
            .style(|s| s.width_pct(100.0).padding(2.0)),
            // The TypstView itself
            container(typst_view).style(|s| {
                s.width_pct(100.0)
                    .height_pct(100.0)
                    .flex_grow(1.0)
            }),
        ))
        .style(|s| {
            s.flex_col()
                .width_pct(100.0)
                .height_pct(100.0)
        }),
    )
    .style(|s| s.width_pct(100.0).height_pct(100.0))
}
