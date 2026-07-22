use std::rc::Rc;

use floem::{
    View,
    reactive::{SignalGet, SignalWith},
    style::CursorStyle,
    views::{Decorators, container, dyn_stack, label, scroll, stack},
};

use super::position::PanelPosition;
use crate::{
    config::color::LapceColor,
    window_tab::WindowTabData,
};

pub fn notes_panel(
    window_tab_data: Rc<WindowTabData>,
    _position: PanelPosition,
) -> impl View {
    let notes = window_tab_data.notes.clone();
    let config = window_tab_data.common.config;
    let typst_view = window_tab_data.typst_view.clone();

    let files = notes.files;
    let content = notes.content;

    // File list
    let notes_for_list = notes.clone();
    let file_list = scroll(
        dyn_stack(
            move || files.get(),
            |f| f.name.clone(),
            move |note_file| {
                let notes = notes_for_list.clone();
                let config = config;
                let name = note_file.name.clone();
                let is_typst = note_file.is_typst;
                let path = note_file.path.clone();

                let file_path = path.clone();
                let is_selected = {
                    let files = notes.files;
                    let selected = notes.selected;
                    let path = path.clone();
                    move || {
                        files.with(|f| {
                            selected.get().map_or(false, |idx| {
                                f.get(idx).map_or(false, |nf| nf.path == path)
                            })
                        })
                    }
                };

                let icon = if is_typst { "◆ " } else { "◇ " };
                let display_name = format!("{}{}", icon, name);

                container(
                    label(move || display_name.clone()).style(move |s| {
                        let config = config.get();
                        s.padding_vert(2.0)
                            .padding_horiz(6.0)
                            .width_pct(100.0)
                            .font_size(config.ui.font_size() as f32)
                            .apply_if(is_selected(), |s| {
                                s.background(
                                    config.color(LapceColor::PANEL_CURRENT_BACKGROUND),
                                )
                            })
                    }),
                )
                .style(|s| s.width_pct(100.0).cursor(CursorStyle::Pointer))
                .on_click_stop(move |_| {
                    let idx = files.with(|f| {
                        f.iter().position(|nf| nf.path == file_path)
                    });
                    if let Some(idx) = idx {
                        notes.select_file(idx);
                    }
                })
            },
        )
        .style(|s| s.flex_col().width_pct(100.0)),
    )
    .style(|s| s.width_pct(100.0).height(200.0));

    // Content preview
    let content_view = scroll(
        container(
            label(move || content.get()).style(move |s| {
                let config = config.get();
                s.padding(6.0)
                    .width_pct(100.0)
                    .font_size(config.ui.font_size() as f32)
            }),
        )
        .style(|s| s.width_pct(100.0)),
    )
    .style(|s| s.width_pct(100.0).flex_grow(1.0));

    // Toolbar with refresh + render buttons
    let notes_for_render = notes.clone();
    let notes_for_refresh = notes.clone();
    let typst_view_for_render = typst_view.clone();
    let toolbar = stack((
        label(|| "Notes".to_string()).style(|s| {
            s.font_bold().padding(4.0)
        }),
        container(label(|| "".to_string())).style(|s| s.flex_grow(1.0)),
        // Render button (for .typ files)
        {
            let notes = notes_for_render.clone();
            let typst_view = typst_view_for_render.clone();
            let notes_for_style = notes_for_render.clone();
            label(|| "⬡ Render".to_string())
                .style(move |s| {
                    let config = config.get();
                    let has_typst =
                        notes_for_style.selected_file().map_or(false, |f| f.is_typst);
                    s.padding_vert(2.0)
                        .padding_horiz(6.0)
                        .cursor(CursorStyle::Pointer)
                        .apply_if(!has_typst, |s| {
                            s.color(config.color(LapceColor::EDITOR_DIM))
                        })
                })
                .on_click_stop(move |_| {
                    notes.render_in_typst_view(&typst_view);
                })
        },
        // Refresh button
        label(|| "↻".to_string())
            .style(|s| {
                s.padding_vert(2.0)
                    .padding_horiz(6.0)
                    .cursor(CursorStyle::Pointer)
            })
            .on_click_stop(move |_| {
                notes_for_refresh.scan_workspace();
            }),
    ))
    .style(|s| s.width_pct(100.0).items_center().padding(2.0));

    container(
        stack((toolbar, file_list, content_view)).style(|s| {
            s.flex_col().width_pct(100.0).height_pct(100.0)
        }),
    )
    .style(|s| s.width_pct(100.0).height_pct(100.0))
}
