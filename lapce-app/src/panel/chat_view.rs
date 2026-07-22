use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::AtomicU64;

use floem::{
    AnyView, IntoView, View,
    event::{Event, EventListener, EventPropagation},
    peniko::Color,
    peniko::kurbo::Point,
    reactive::{
        RwSignal, SignalGet, SignalUpdate, SignalWith, create_memo,
        create_rw_signal,
    },
    style::CursorStyle,
    text::Style as FontStyle,
    views::{
        Decorators, container, dyn_stack, empty, label, rich_text, scroll, stack,
    },
};

use super::{kind::PanelKind, position::PanelPosition};
use crate::{
    chat::{
        ChatBlock, ChatData, ChatDiffLineKind, ToolStatus, diff_lines,
        extract_file_path,
    },
    command::{InternalCommand, LapceWorkbenchCommand},
    config::color::LapceColor,
    editor::view::editor_view,
    editor_tab::EditorTabChild,
    markdown::{MarkdownContent, parse_markdown},
    window_tab::{Focus, WindowTabData},
};

/// Whether a `chat_view` is the docked panel chat or an independent
/// editor-tab chat. They differ in how they claim keyboard focus: the panel
/// chat uses `Focus::Panel(Chat)`, while an editor-tab chat lives under
/// `Focus::Workbench` (its keys are routed by checking the active editor-tab
/// child in `window_tab::key_down`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChatViewKind {
    Panel,
    EditorTab(crate::id::ChatId),
}

pub fn chat_panel(
    window_tab_data: Rc<WindowTabData>,
    _position: PanelPosition,
) -> impl View {
    let chat = window_tab_data.chat.clone();
    chat_view(chat, window_tab_data, ChatViewKind::Panel)
}

/// Render a single chat instance. Used both by the chat panel (the
/// `window_tab_data.chat` singleton) and by editor-tab chats (each their own
/// `ChatData`).
pub fn chat_view(
    chat: ChatData,
    window_tab_data: Rc<WindowTabData>,
    kind: ChatViewKind,
) -> impl View {
    let config = window_tab_data.common.config;
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

    let view = stack((
        // Header row — "+ New Chat" opens an independent chat as an editor tab.
        {
            let workbench_command = window_tab_data.common.workbench_command;
            container(
                label(|| "+ New Chat".to_string())
                    .on_click_stop(move |_| {
                        workbench_command
                            .send(LapceWorkbenchCommand::NewChatTab);
                    })
                    .style(move |s| {
                        let config = config.get();
                        s.padding_horiz(10.0)
                            .padding_vert(4.0)
                            .items_center()
                            .justify_center()
                            .border(1.0)
                            .border_radius(6.0)
                            .font_size(12.0)
                            .cursor(CursorStyle::Pointer)
                            .selectable(false)
                            .color(config.color(LapceColor::EDITOR_FOREGROUND))
                            .border_color(config.color(LapceColor::LAPCE_BORDER))
                            .hover(|s| {
                                s.background(
                                    config
                                        .color(LapceColor::PANEL_HOVERED_BACKGROUND),
                                )
                            })
                    }),
            )
            .style(|s| s.width_pct(100.0).justify_end().padding(8.0))
        },
        // Message list — one continuous scroll, no borders between blocks.
        // 16px padding insets every block (markdown, tool fixtures) from the
        // panel edge, matching crow-ade's `.sc-messages { padding: 16px }`.
        container({
            scroll({
                dyn_stack(
                    move || chat_for_stack.blocks.get(),
                    |block| block.id(),
                    move |block| {
                        render_block(block, config, chat_for_stack.clone())
                    },
                )
                .style(|s| s.flex_col().width_pct(100.0).padding(16.0))
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
        // Input area — a real multi-line lapce editor. Enter sends,
        // Shift+Enter inserts a newline (wired via ChatInputFocus + the
        // Focus::Panel(Chat) key route in window_tab).
        {
            let chat_for_click = chat.clone();
            let input_editor = chat.input_editor.clone();
            let debug_breakline =
                create_memo(move |_| None::<(usize, std::path::PathBuf)>);
            // Copy signals captured by the `is_active` closure (it must be
            // `Copy`, so it can't hold the `Rc<WindowTabData>`).
            let active_editor_tab =
                window_tab_data.main_split.active_editor_tab;
            let editor_tabs = window_tab_data.main_split.editor_tabs;
            let is_active = move |tracked: bool| {
                let f = if tracked {
                    focus.get()
                } else {
                    focus.get_untracked()
                };
                match kind {
                    ChatViewKind::Panel => f == Focus::Panel(PanelKind::Chat),
                    ChatViewKind::EditorTab(id) => {
                        if f != Focus::Workbench {
                            return false;
                        }
                        // Active iff this chat is the active child of the
                        // active editor tab (read tracked so the editor-view
                        // memo re-evaluates on tab switches).
                        let Some(tab_id) = active_editor_tab.get() else {
                            return false;
                        };
                        let Some(tab) = editor_tabs
                            .with(|tabs| tabs.get(&tab_id).copied())
                        else {
                            return false;
                        };
                        tab.with(|t| {
                            matches!(
                                t.children.get(t.active),
                                Some((_, _, EditorTabChild::Chat(cid)))
                                    if *cid == id
                            )
                        })
                    }
                }
            };
            // Drag handle above the editor: drag up = taller input.
            // Mirrors lapce's split-divider drag (app.rs): request_active
            // on PointerDown keeps PointerMove firing while the cursor
            // leaves the thin handle.
            let input_height = chat.input_height;
            let drag_start: RwSignal<Option<(Point, f64)>> =
                create_rw_signal(None);
            let handle = empty();
            let handle_id = handle.id();
            let drag_handle = handle
                .on_event_stop(EventListener::PointerDown, move |event| {
                    handle_id.request_active();
                    if let Event::PointerDown(pointer_event) = event {
                        drag_start.set(Some((
                            pointer_event.pos,
                            input_height.get_untracked(),
                        )));
                    }
                })
                .on_event_stop(EventListener::PointerUp, move |_| {
                    drag_start.set(None);
                })
                .on_event_stop(EventListener::PointerMove, move |event| {
                    if let Event::PointerMove(pointer_event) = event {
                        if let Some((start_pt, start_h)) =
                            drag_start.get_untracked()
                        {
                            // Dragging up (smaller y) grows the input.
                            let new_h = start_h + (start_pt.y - pointer_event.pos.y);
                            input_height.set(new_h.clamp(80.0, 600.0));
                        }
                    }
                })
                .style(move |s| {
                    let config = config.get();
                    let is_dragging = drag_start.get().is_some();
                    s.width_pct(100.0)
                        .height(5.0)
                        .margin_bottom(2.0)
                        .cursor(CursorStyle::RowResize)
                        .apply_if(is_dragging, |s| {
                            s.background(config.color(LapceColor::EDITOR_CARET))
                        })
                        .hover(|s| {
                            s.background(config.color(LapceColor::LAPCE_BORDER))
                        })
                });
            stack((
                drag_handle,
                container(editor_view(input_editor, debug_breakline, is_active))
                    .style(move |s| {
                        let config = config.get();
                        s.width_pct(100.0)
                            .height(input_height.get())
                            .border(1.0)
                            .border_radius(8.0)
                            .border_color(config.color(LapceColor::LAPCE_BORDER))
                            .background(config.color(LapceColor::EDITOR_BACKGROUND))
                            .font_size(13.0)
                    }),
                label(|| "Send".to_string())
                    .on_click_stop(move |_| {
                        chat_for_click.send_prompt();
                    })
                    .style(move |s| {
                        let config = config.get();
                        s.margin_top(6.0)
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
            .style(|s| {
                s.flex_col()
                    .items_start()
                    .width_pct(100.0)
                    // Match the message list's 16px horizontal margins so the
                    // input doesn't butt against the panel edge either.
                    .padding_horiz(16.0)
                    .padding_vert(12.0)
            })
        },
    ))
    .style(|s| s.flex_col().size_pct(100.0, 100.0))
    .debug_name("Chat Panel");

    // Claim keyboard focus on click. The panel chat owns `Focus::Panel(Chat)`
    // (and stops propagation). An editor-tab chat lives under
    // `Focus::Workbench`: we let the click bubble up to the editor-tab
    // container (app.rs), which sets `Focus::Workbench` and activates this
    // tab — `window_tab::key_down` then routes keys to this chat's input.
    match kind {
        ChatViewKind::Panel => {
            view.on_event_stop(EventListener::PointerDown, move |_| {
                if focus.get_untracked() != Focus::Panel(PanelKind::Chat) {
                    focus.set(Focus::Panel(PanelKind::Chat));
                }
            })
        }
        ChatViewKind::EditorTab(_) => view,
    }
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
            diff_path,
            old_text,
            new_text,
            ..
        } => render_tool_call(
            title, kind, status, raw_input, raw_output, terminal_id,
            diff_path, old_text, new_text, config, chat.clone(),
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

/// Strip a leading `read:`/`write:`/`edit:` (or space) prefix from a tool
/// title, mirroring crow-ade's `toolCallItem` (the path is shown as a link
/// instead, so the prefix is redundant noise).
fn strip_tool_prefix(title: &str) -> String {
    let lower = title.to_lowercase();
    for prefix in ["read:", "write:", "edit:", "read ", "write ", "edit "] {
        if lower.starts_with(prefix) {
            return title[prefix.len()..].trim().to_string();
        }
    }
    title.to_string()
}

/// Tool call — compact card with border, matching crow-ade's .sc-tool-call.
fn render_tool_call(
    title: String,
    kind: String,
    status: ToolStatus,
    raw_input: Option<String>,
    raw_output: Option<String>,
    terminal_id: Option<String>,
    diff_path: Option<String>,
    old_text: Option<String>,
    new_text: Option<String>,
    config: floem::reactive::ReadSignal<std::sync::Arc<crate::config::LapceConfig>>,
    chat: ChatData,
) -> impl View {
    let open = create_rw_signal(true);
    let status_icon = status.icon().to_string();
    let kind_label = kind.clone();

    // File path for a clickable "open file" link (edit/write carry it on the
    // diff block; read/etc. carry it in their rawInput args).
    let file_path =
        extract_file_path(diff_path.as_deref(), raw_input.as_deref());
    let is_link = file_path.is_some();
    // Read tools render ONLY a clickable link — never dump the file body.
    let is_read = kind == "read" || title.to_lowercase().starts_with("read");
    // Header text: the file path when we have one (rendered as a clickable
    // link), else the title with any read:/write:/edit: prefix stripped.
    let header_text = match &file_path {
        Some(p) => p.clone(),
        None => strip_tool_prefix(&title),
    };
    let internal_command = chat.common.internal_command;
    let path_for_click = file_path.clone();

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
                label(move || header_text.clone())
                    .on_click(move |_| {
                        if let Some(p) = &path_for_click {
                            internal_command.send(InternalCommand::OpenFile {
                                path: PathBuf::from(p),
                            });
                            EventPropagation::Stop
                        } else {
                            // Not a file link — let the click bubble up to the
                            // header's collapse toggle.
                            EventPropagation::Continue
                        }
                    })
                    .style(move |s| {
                        let config = config.get();
                        s.flex_grow(1.0)
                            .flex_basis(0.0)
                            .font_size(11.0)
                            .font_family(config.editor.font_family.clone())
                            .color(config.color(if is_link {
                                LapceColor::EDITOR_LINK
                            } else {
                                LapceColor::EDITOR_FOREGROUND
                            }))
                            .text_ellipsis()
                            .selectable(false)
                            .apply_if(is_link, |s| {
                                s.cursor(CursorStyle::Pointer)
                            })
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
            } else if let Some(new_text) = new_text {
                // edit/write tool: render an inline diff (edit = old→new;
                // write = old_text None → all-added new-file view).
                container(render_diff_content(old_text, new_text, config))
                    .style(move |s| {
                        s.width_pct(100.0)
                            .apply_if(!open.get(), |s| s.hide())
                    })
                    .into_any()
            } else if is_read {
                // Read tool: show nothing but the clickable path link in the
                // header — never dump the file body into the chat.
                empty().into_any()
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

/// Render an inline diff for an edit/write tool call. `old_text` None ⇒ a
/// Render the inline diff body for an edit/write tool call. `old_text` None
/// ⇒ a brand-new file (write): every line renders as "added". The clickable
/// path header lives in the tool fixture header (see `render_tool_call`).
fn render_diff_content(
    old_text: Option<String>,
    new_text: String,
    config: floem::reactive::ReadSignal<std::sync::Arc<crate::config::LapceConfig>>,
) -> impl View {
    let lines: Vec<(usize, crate::chat::ChatDiffLine)> =
        diff_lines(&old_text.unwrap_or_default(), &new_text)
            .into_iter()
            .enumerate()
            .collect();

    container(
        dyn_stack(
            move || lines.clone(),
            |(i, _)| *i,
            move |(_, line)| diff_line_view(line, config),
        )
        .style(|s| s.flex_col().width_pct(100.0)),
    )
    .style(|s| s.width_pct(100.0).padding_bottom(6.0))
}

/// One line of the inline diff: a `+`/`-`/` ` marker + text, tinted
/// green (added) / red (removed) / plain (context).
fn diff_line_view(
    line: crate::chat::ChatDiffLine,
    config: floem::reactive::ReadSignal<std::sync::Arc<crate::config::LapceConfig>>,
) -> impl View {
    let kind = line.kind;
    let marker = match kind {
        ChatDiffLineKind::Added => "+",
        ChatDiffLineKind::Removed => "-",
        ChatDiffLineKind::Context => " ",
    }
    .to_string();
    let text = line.text;

    label(move || format!("{marker} {text}"))
        .style(move |s| {
            let config = config.get();
            let (fg, bg) = match kind {
                ChatDiffLineKind::Added => {
                    let c = config
                        .color(LapceColor::SOURCE_CONTROL_ADDED)
                        .to_rgba8();
                    (
                        config.color(LapceColor::SOURCE_CONTROL_ADDED),
                        Color::from_rgba8(c.r, c.g, c.b, 28),
                    )
                }
                ChatDiffLineKind::Removed => {
                    let c = config
                        .color(LapceColor::SOURCE_CONTROL_REMOVED)
                        .to_rgba8();
                    (
                        config.color(LapceColor::SOURCE_CONTROL_REMOVED),
                        Color::from_rgba8(c.r, c.g, c.b, 28),
                    )
                }
                ChatDiffLineKind::Context => (
                    config.color(LapceColor::EDITOR_FOREGROUND),
                    Color::TRANSPARENT,
                ),
            };
            s.width_pct(100.0)
                .font_size(12.0)
                .font_family(config.editor.font_family.clone())
                .color(fg)
                .background(bg)
                .padding_horiz(10.0)
                .selectable(true)
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
