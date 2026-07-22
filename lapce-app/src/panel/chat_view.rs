use std::rc::Rc;
use std::sync::atomic::AtomicU64;

use floem::{
    AnyView, IntoView, View,
    event::{Event, EventListener},
    keyboard::{Key, NamedKey},
    reactive::{RwSignal, SignalGet, SignalUpdate, create_rw_signal},
    style::CursorStyle,
    text::Style as FontStyle,
    views::{
        Decorators, container, dyn_stack, empty, label, rich_text, scroll,
        stack, text_input,
    },
};

use super::{kind::PanelKind, position::PanelPosition};
use crate::{
    chat::{ChatBlock, ChatData, ToolStatus},
    config::color::LapceColor,
    markdown::{MarkdownContent, parse_markdown},
    window_tab::{Focus, WindowTabData},
};

pub fn chat_panel(
    window_tab_data: Rc<WindowTabData>,
    _position: PanelPosition,
) -> impl View {
    let config = window_tab_data.common.config;
    let chat = window_tab_data.chat.clone();
    let focus = window_tab_data.common.focus;
    let is_loading = chat.is_loading;
    let scroll_version = chat.scroll_version;

    let scroll_target = create_rw_signal(None::<floem::kurbo::Point>);

    {
        let scroll_target = scroll_target;
        floem::reactive::create_effect(move |_| {
            let _v = scroll_version.get();
            scroll_target.set(Some(floem::kurbo::Point::new(0.0, 1e9)));
        });
    }

    let chat_for_stack = chat.clone();

    stack((
        // Message list — one continuous scroll, no borders between blocks
        container({
            scroll({
                dyn_stack(
                    move || chat_for_stack.blocks.get(),
                    |block| block.id(),
                    move |block| {
                        render_block(block, config, chat_for_stack.clone())
                    },
                )
                .style(|s| s.flex_col().width_pct(100.0))
            })
            .scroll_to(move || scroll_target.get())
            .style(|s| s.absolute().size_pct(100.0, 100.0))
        })
        .style(|s| s.size_pct(100.0, 100.0).flex_grow(1.0).flex_basis(0.0)),
        // Streaming indicator + Stop button row
        {
            let chat_for_stop = chat.clone();
            stack((
                label(move || {
                    if is_loading.get() {
                        "● ● ●"
                    } else {
                        ""
                    }
                    .to_string()
                })
                .style(move |s| {
                    let config = config.get();
                    s.flex_grow(1.0)
                        .color(config.color(LapceColor::EDITOR_DIM))
                        .font_size(11.0)
                        .apply_if(!is_loading.get(), |s| s.hide())
                        .selectable(false)
                }),
                label(|| "■ Stop".to_string())
                    .on_click_stop(move |_| {
                        chat_for_stop.cancel_prompt();
                    })
                    .style(move |s| {
                        let config = config.get();
                        s.padding_horiz(10.0)
                            .padding_vert(2.0)
                            .border(1.0)
                            .border_radius(4.0)
                            .border_color(config.color(LapceColor::LAPCE_ERROR))
                            .color(config.color(LapceColor::LAPCE_ERROR))
                            .cursor(CursorStyle::Pointer)
                            .selectable(false)
                            .apply_if(!is_loading.get(), |s| s.hide())
                            .hover(|s| {
                                s.background(
                                    config
                                        .color(LapceColor::PANEL_HOVERED_BACKGROUND),
                                )
                            })
                    }),
            ))
            .style(move |s| {
                s.width_pct(100.0)
                    .padding_horiz(16.0)
                    .padding_vert(4.0)
                    .items_center()
                    .apply_if(!is_loading.get(), |s| s.hide())
            })
        },
        // Input area
        {
            let chat_for_key = chat.clone();
            let chat_for_click = chat.clone();
            let input_text = chat.input;
            stack((
                text_input(input_text)
                    .placeholder("Ask anything...")
                    .on_event_stop(EventListener::KeyDown, move |event| {
                        if let Event::KeyDown(key_event) = event {
                            if key_event.key.logical_key
                                == Key::Named(NamedKey::Enter)
                            {
                                chat_for_key.send_prompt();
                            }
                        }
                    })
                    .style(move |s| {
                        let config = config.get();
                        s.width_pct(100.0)
                            .padding(10.0)
                            .border(1.0)
                            .border_radius(8.0)
                            .border_color(config.color(LapceColor::LAPCE_BORDER))
                            .background(config.color(LapceColor::EDITOR_BACKGROUND))
                            .color(config.color(LapceColor::EDITOR_FOREGROUND))
                            .font_size(13.0)
                    }),
                label(|| "Send".to_string())
                    .on_click_stop(move |_| {
                        chat_for_click.send_prompt();
                    })
                    .style(move |s| {
                        let config = config.get();
                        s.margin_left(6.0)
                            .padding_horiz(12.0)
                            .padding_vert(6.0)
                            .items_center()
                            .justify_center()
                            .border(1.0)
                            .border_radius(6.0)
                            .border_color(config.color(LapceColor::LAPCE_BORDER))
                            .cursor(CursorStyle::Pointer)
                            .selectable(false)
                            .apply_if(chat.is_loading.get(), |s| s.hide())
                            .hover(|s| {
                                s.background(
                                    config
                                        .color(LapceColor::PANEL_HOVERED_BACKGROUND),
                                )
                            })
                    }),
            ))
            .style(|s| s.width_pct(100.0).padding(12.0).items_center())
        },
    ))
    .on_event_stop(EventListener::PointerDown, move |_| {
        if focus.get_untracked() != Focus::Panel(PanelKind::Chat) {
            focus.set(Focus::Panel(PanelKind::Chat));
        }
    })
    .style(|s| s.flex_col().size_pct(100.0, 100.0))
    .debug_name("Chat Panel")
}

fn render_block(
    block: ChatBlock,
    config: floem::reactive::ReadSignal<std::sync::Arc<crate::config::LapceConfig>>,
    chat: ChatData,
) -> AnyView {
    match block {
        ChatBlock::UserText { text, .. } => {
            render_user_text(text, config).into_any()
        }
        ChatBlock::AssistantText { text, .. } => {
            render_assistant_text(text, config).into_any()
        }
        ChatBlock::Thinking { text, open, .. } => {
            render_thinking_block(text, open, config).into_any()
        }
        ChatBlock::ToolCall {
            title,
            kind,
            status,
            raw_input,
            raw_output,
            terminal_id,
            ..
        } => render_tool_call(
            title, kind, status, raw_input, raw_output, terminal_id,
            config, chat.clone(),
        )
        .into_any(),
        ChatBlock::System { text, .. } => {
            render_system_text(text, config).into_any()
        }
    }
}

/// User message — rounded box with subtle background, no header.
fn render_user_text(
    text: String,
    config: floem::reactive::ReadSignal<std::sync::Arc<crate::config::LapceConfig>>,
) -> impl View {
    container(
        label(move || text.clone()).style(move |s| {
            let config = config.get();
            s.width_pct(100.0)
                .color(config.color(LapceColor::EDITOR_FOREGROUND))
                .font_size(13.0)
                .line_height(1.6)
                .selectable(true)
        }),
    )
    .style(move |s| {
        let config = config.get();
        s.width_pct(100.0)
            .padding(10.0)
            .padding_horiz(14.0)
            .margin_bottom(12.0)
            .border(1.0)
            .border_radius(8.0)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .background(config.color(LapceColor::EDITOR_BACKGROUND))
    })
}

/// Agent message — markdown flows directly, no header, no border.
fn render_assistant_text(
    text: RwSignal<String>,
    config: floem::reactive::ReadSignal<std::sync::Arc<crate::config::LapceConfig>>,
) -> impl View {
    let md_id = AtomicU64::new(0);
    container(
        dyn_stack(
            move || parse_markdown(&text.get(), 1.5, &config.get()),
            move |_| md_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            move |content| match content {
                MarkdownContent::Text(text_layout) => container(
                    rich_text(move || text_layout.clone())
                        .style(|s| s.width_pct(100.0)),
                )
                .style(|s| s.width_pct(100.0)),
                MarkdownContent::Image { .. } => container(empty()),
                MarkdownContent::Separator => container(empty().style(move |s| {
                    let config = config.get();
                    s.width_pct(100.0)
                        .margin_vert(5.0)
                        .height(1.0)
                        .background(config.color(LapceColor::LAPCE_BORDER))
                })),
            },
        )
        .style(|s| s.flex_col().width_pct(100.0)),
    )
    .style(move |s| {
        s.width_pct(100.0)
            .padding_vert(4.0)
            .margin_bottom(8.0)
    })
}

/// Thinking — single dim line, collapsible.
fn render_thinking_block(
    text: RwSignal<String>,
    open: RwSignal<bool>,
    config: floem::reactive::ReadSignal<std::sync::Arc<crate::config::LapceConfig>>,
) -> impl View {
    stack((
        stack((
            label(move || {
                if open.get() {
                    "▾"
                } else {
                    "▸"
                }
                .to_string()
            })
            .style(move |s| {
                let config = config.get();
                s.margin_right(4.0)
                    .font_size(10.0)
                    .color(config.color(LapceColor::EDITOR_DIM))
                    .selectable(false)
            }),
            label(|| "thinking".to_string()).style(move |s| {
                let config = config.get();
                s.font_size(11.0)
                    .font_style(FontStyle::Italic)
                    .color(config.color(LapceColor::EDITOR_DIM))
                    .selectable(false)
            }),
        ))
        .style(move |s| {
            let config = config.get();
            s.padding_vert(2.0)
                .padding_horiz(4.0)
                .cursor(CursorStyle::Pointer)
                .hover(|s| {
                    s.background(config.color(LapceColor::PANEL_HOVERED_BACKGROUND))
                })
        })
        .on_click_stop(move |_| {
            open.update(|o| *o = !*o);
        }),
        label(move || text.get()).style(move |s| {
            let config = config.get();
            s.width_pct(100.0)
                .padding(6.0)
                .padding_left(20.0)
                .font_size(11.0)
                .font_style(FontStyle::Italic)
                .color(config.color(LapceColor::EDITOR_DIM))
                .selectable(true)
                .apply_if(!open.get(), |s| s.hide())
        }),
    ))
    .style(move |s| {
        s.flex_col()
            .width_pct(100.0)
            .margin_bottom(4.0)
    })
}

/// Tool call — compact card with border, matching crow-ade's .sc-tool-call.
fn render_tool_call(
    title: String,
    kind: String,
    status: ToolStatus,
    raw_input: Option<String>,
    raw_output: Option<String>,
    terminal_id: Option<String>,
    config: floem::reactive::ReadSignal<std::sync::Arc<crate::config::LapceConfig>>,
    chat: ChatData,
) -> impl View {
    let open = create_rw_signal(true);
    let status_icon = status.icon().to_string();
    let kind_label = kind.clone();
    let title_label = title.clone();
    let input_text = raw_input.unwrap_or_default();
    let output_text = raw_output.unwrap_or_default();

    let term_handle = terminal_id
        .as_ref()
        .and_then(|tid| chat.terminals.borrow().get(tid).cloned());

    container(
        stack((
            // Header bar
            stack((
                label(move || status_icon.clone()).style(move |s| {
                    let config = config.get();
                    let color = match status {
                        ToolStatus::Completed => {
                            config.color(LapceColor::SOURCE_CONTROL_ADDED)
                        }
                        ToolStatus::Failed => {
                            config.color(LapceColor::LAPCE_ERROR)
                        }
                        ToolStatus::InProgress => {
                            config.color(LapceColor::EDITOR_DIM)
                        }
                        ToolStatus::Pending => {
                            config.color(LapceColor::EDITOR_DIM)
                        }
                    };
                    s.margin_right(6.0)
                        .font_size(11.0)
                        .color(color)
                        .selectable(false)
                }),
                label(move || kind_label.clone()).style(move |s| {
                    let config = config.get();
                    s.font_size(11.0)
                        .font_family(config.editor.font_family.clone())
                        .color(config.color(LapceColor::EDITOR_FOREGROUND))
                        .margin_right(8.0)
                        .selectable(false)
                }),
                label(move || title_label.clone()).style(move |s| {
                    let config = config.get();
                    s.flex_grow(1.0)
                        .flex_basis(0.0)
                        .font_size(11.0)
                        .font_family(config.editor.font_family.clone())
                        .color(config.color(LapceColor::EDITOR_LINK))
                        .text_ellipsis()
                        .selectable(false)
                }),
                label(move || {
                    if open.get() {
                        "▾"
                    } else {
                        "▸"
                    }
                    .to_string()
                })
                .style(move |s| {
                    let config = config.get();
                    s.font_size(10.0)
                        .color(config.color(LapceColor::EDITOR_DIM))
                        .selectable(false)
                }),
            ))
            .style(move |s| {
                let config = config.get();
                s.width_pct(100.0)
                    .padding_vert(4.0)
                    .padding_horiz(8.0)
                    .items_center()
                    .cursor(CursorStyle::Pointer)
                    .hover(|s| {
                        s.background(
                            config.color(LapceColor::PANEL_HOVERED_BACKGROUND),
                        )
                    })
            })
            .on_click_stop(move |_| {
                open.update(|o| *o = !*o);
            }),
            // Content area
            if let Some(handle) = term_handle {
                container(
                    crate::chat_terminal::chat_terminal_view(handle, config)
                        .style(|s| s.width_pct(100.0).height(200.0)),
                )
                .style(move |s| {
                    s.width_pct(100.0)
                        .padding(8.0)
                        .padding_horiz(10.0)
                        .apply_if(!open.get(), |s| s.hide())
                })
                .into_any()
            } else {
                let input_empty = input_text.is_empty();
                let output_empty = output_text.is_empty();
                let input_for_label = input_text.clone();
                let output_for_label = output_text.clone();
                stack((
                    label(move || input_for_label.clone()).style(move |s| {
                        let config = config.get();
                        s.width_pct(100.0)
                            .font_size(11.0)
                            .font_family(config.editor.font_family.clone())
                            .color(config.color(LapceColor::EDITOR_FOREGROUND))
                            .padding(4.0)
                            .border_radius(3.0)
                            .selectable(true)
                            .apply_if(input_empty, |s| s.hide())
                    }),
                    label(move || output_for_label.clone()).style(move |s| {
                        let config = config.get();
                        s.width_pct(100.0)
                            .font_size(11.0)
                            .font_family(config.editor.font_family.clone())
                            .color(config.color(LapceColor::EDITOR_FOREGROUND))
                            .padding(4.0)
                            .border_radius(3.0)
                            .selectable(true)
                            .apply_if(output_empty, |s| s.hide())
                    }),
                ))
                .style(move |s| {
                    s.flex_col()
                        .width_pct(100.0)
                        .padding(8.0)
                        .padding_horiz(10.0)
                        .apply_if(!open.get(), |s| s.hide())
                })
                .into_any()
            },
        ))
        .style(|s| s.flex_col().width_pct(100.0)),
    )
    .style(move |s| {
        let config = config.get();
        s.width_pct(100.0)
            .margin_vert(4.0)
            .border(1.0)
            .border_radius(6.0)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
            .background(config.color(LapceColor::EDITOR_BACKGROUND))
    })
}

fn render_system_text(
    text: String,
    config: floem::reactive::ReadSignal<std::sync::Arc<crate::config::LapceConfig>>,
) -> impl View {
    label(move || text.clone()).style(move |s| {
        let config = config.get();
        s.width_pct(100.0)
            .padding_vert(4.0)
            .font_size(11.0)
            .font_style(FontStyle::Italic)
            .color(config.color(LapceColor::EDITOR_DIM))
            .selectable(true)
    })
}
