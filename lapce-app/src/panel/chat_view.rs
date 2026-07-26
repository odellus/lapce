use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::AtomicU64;

use floem::{
    AnyView, IntoView, View,
    event::{Event, EventListener, EventPropagation},
    peniko::Color,
    peniko::kurbo::{Point, Rect},
    reactive::{
        RwSignal, SignalGet, SignalTrack, SignalUpdate, SignalWith, create_memo,
        create_rw_signal,
    },
    style::CursorStyle,
    text::Style as FontStyle,
    views::{
        Decorators, container, dyn_stack,
        editor::{WrapProp, text::WrapMethod, view::{LineRegion, cursor_caret}},
        empty, label, rich_text, scroll, stack,
    },
};

use super::{kind::PanelKind, position::PanelPosition};
use crate::{
    chat::{
        ChatBlock, ChatData, ChatDiffLineKind, ToolStatus, diff_lines,
        extract_file_path, parse_model_select,
    },
    command::InternalCommand,
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

/// Which chat dropdown popup is currently open (at most one at a time). The
/// agent/model/history lists are rendered as absolute overlays (see the
/// dropdown overlay in `chat_view`) so opening one never pushes the
/// surrounding layout — the history list in particular is a scrollable modal.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ChatDropdown {
    None,
    Agent,
    Model,
    History,
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
    let auto_scroll = chat.auto_scroll;

    let scroll_target = create_rw_signal(None::<floem::kurbo::Point>);
    // Track the furthest-down scroll position we've seen (approximates
    // the content bottom from the last auto-scroll).
    let max_scroll_y = create_rw_signal(0.0_f64);

    {
        let scroll_target = scroll_target;
        let auto_scroll = auto_scroll;
        floem::reactive::create_effect(move |_| {
            let _v = scroll_version.get();
            if auto_scroll.get_untracked() {
                scroll_target.set(Some(floem::kurbo::Point::new(0.0, 1e9)));
            }
        });
    }

    let chat_for_stack = chat.clone();

    // Shared dropdown state: which popup (agent/model/history) is open. The
    // lists render as absolute overlays so they never push the layout.
    let dropdown: RwSignal<ChatDropdown> = create_rw_signal(ChatDropdown::None);
    // Agent names read once at build (settings aren't hot-reloaded).
    let agent_names: Vec<String> = config
        .get_untracked()
        .acp
        .agents
        .iter()
        .map(|a| a.name.clone())
        .collect();

    let view = stack((
        // Header — status dot + session id (left) and the history trigger
        // (right). The agent/model selectors live in the bottom toolbar; all
        // dropdown lists render as overlays (see below) so they never push the
        // layout.
        {
            let session_id_sig = chat.session_id;
            let sessions_sig = chat.sessions;
            let chat_for_hist_req = chat.clone();
            // session id (left) ............ history trigger (right)
            stack((
                    stack((
                        // Status dot: green when a session is connected.
                        container(empty()).style(move |s| {
                            let connected = session_id_sig.get().is_some();
                            s.size(8.0, 8.0).border_radius(4.0).background(
                                if connected {
                                    Color::from_rgba8(0x4e, 0xc2, 0x4e, 255)
                                } else {
                                    Color::from_rgba8(0x80, 0x80, 0x80, 255)
                                },
                            )
                        }),
                        label(move || {
                            session_id_sig
                                .get()
                                .unwrap_or_else(|| "No session".to_string())
                        })
                        .style(|s| {
                            s.font_size(11.0)
                                .margin_left(6.0)
                                .selectable(true)
                                .color(Color::from_rgba8(0x9a, 0x9a, 0x9a, 255))
                        }),
                    ))
                    .style(|s| s.flex_row().items_center()),
                    empty().style(|s| s.flex_grow(1.0)),
                    label(move || {
                        format!("history ▾ ({})", sessions_sig.get().len())
                    })
                    .on_click_stop(move |_| {
                        let opening =
                            dropdown.get_untracked() != ChatDropdown::History;
                        dropdown.set(if opening {
                            ChatDropdown::History
                        } else {
                            ChatDropdown::None
                        });
                        if opening {
                            chat_for_hist_req.request_session_list();
                        }
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
                ))
                .style(move |s| {
                    let config = config.get();
                    s.flex_row()
                        .items_center()
                        .width_pct(100.0)
                        .padding(8.0)
                        .border_bottom(1.0)
                        .border_color(config.color(LapceColor::LAPCE_BORDER))
                })
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
            .on_scroll(move |rect| {
                // Track whether the viewport is at the bottom.
                // When the user scrolls up, y1 drops below the max we've
                // seen (which approximates the content bottom from the
                // last auto-scroll).  When they scroll back down to the
                // bottom, y1 catches up again.
                let y1 = rect.y1;
                let prev = max_scroll_y.get_untracked();
                if y1 > prev {
                    max_scroll_y.set(y1);
                }
                let current_max = max_scroll_y.get_untracked();
                auto_scroll.set(y1 >= current_max - 40.0);
            })
            // Absorb the wheel even at the scroll extremes so the chat scroll
            // "takes control" instead of leaking to the outer workbench scroll
            // (mirrors crow-ade's wheel stopPropagation on its message list).
            // floem's scroll view only stops the wheel when it actually moves;
            // with the default `propagate_pointer_wheel = true` the wheel leaks
            // to the parent at the top/bottom, scrolling the whole IDE. The
            // setting lives on `ScrollCustomStyle`, applied via `.scroll_style`
            // (same pattern lapce uses for `.hide_bars`).
            .scroll_style(|s| s.propagate_pointer_wheel(false))
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
        // Queue indicator — shows queued prompt items above the input.
        {
            let queued = chat.queued_items;
            dyn_stack(
                move || {
                    let items = queued.get();
                    let arr = items.as_array().cloned().unwrap_or_default();
                    arr.into_iter().enumerate().collect::<Vec<_>>()
                },
                |(i, _)| *i,
                move |(_i, item)| {
                    let text = item
                        .as_array()
                        .and_then(|blocks| blocks.first())
                        .and_then(|b| b.get("text"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("(queued prompt)")
                        .to_string();
                    let display = if text.len() > 60 {
                        format!("{}…", &text[..57])
                    } else {
                        text
                    };
                    label(move || format!("⏳ {}", display.clone()))
                        .style(move |s| {
                            let config = config.get();
                            s.width_pct(100.0)
                                .padding_horiz(16.0)
                                .padding_vert(2.0)
                                .font_size(11.0)
                                .color(config.color(LapceColor::EDITOR_DIM))
                                .text_ellipsis()
                                .border_bottom(1.0)
                                .border_color(config.color(LapceColor::LAPCE_BORDER))
                        })
                },
            )
            .style(|s| s.width_pct(100.0).flex_col())
        },
        // Task list — compact inline display of worker/orchestrator tasks.
        {
            let task_list_sig = chat.task_list;
            let orch_task_list_sig = chat.orchestrator_task_list;
            dyn_stack(
                move || {
                    let mut rows: Vec<(String, String, String)> = Vec::new();
                    // Worker tasks
                    let tasks = task_list_sig.get();
                    if let Some(arr) = tasks.as_array() {
                        for t in arr {
                            let title = t.get("title").and_then(|v| v.as_str()).unwrap_or("?").to_string();
                            let status = t.get("status").and_then(|v| v.as_str()).unwrap_or("pending").to_string();
                            rows.push(("task".to_string(), title, status));
                        }
                    }
                    // Orchestrator tasks
                    let orch = orch_task_list_sig.get();
                    if let Some(arr) = orch.as_array() {
                        for t in arr {
                            let title = t.get("title").and_then(|v| v.as_str()).unwrap_or("?").to_string();
                            let status = t.get("status").and_then(|v| v.as_str()).unwrap_or("pending").to_string();
                            rows.push(("orch".to_string(), title, status));
                        }
                    }
                    rows.into_iter().enumerate().collect::<Vec<_>>()
                },
                |(i, _)| *i,
                move |(_, (kind, title, status))| {
                    let icon = match status.as_str() {
                        "completed" => "✓",
                        "failed" => "✗",
                        "cancelled" => "⊘",
                        "in_progress" => "▶",
                        "delegated" => "⇢",
                        _ => "○",
                    };
                    let prefix = if kind == "orch" { "⚙ " } else { "" };
                    label(move || format!("{}{}{} {}", icon, " ", prefix, title.clone()))
                        .style(move |s| {
                            let config = config.get();
                            let color = match status.as_str() {
                                "completed" => Color::from_rgba8(0x4e, 0xc2, 0x4e, 255),
                                "failed" => config.color(LapceColor::LAPCE_ERROR),
                                "in_progress" => config.color(LapceColor::EDITOR_FOREGROUND),
                                "delegated" => Color::from_rgba8(0x56, 0x9c, 0xd6, 255),
                                _ => config.color(LapceColor::EDITOR_DIM),
                            };
                            s.width_pct(100.0)
                                .padding_horiz(16.0)
                                .padding_vert(1.0)
                                .font_size(11.0)
                                .color(color)
                                .text_ellipsis()
                        })
                },
            )
            .style(|s| s.width_pct(100.0).flex_col())
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
                container({
                    // Reference pattern (editor/view.rs: normal editor): a
                    // `scroll` gives the editor a *definite* viewport from the
                    // parent chain; `absolute()` + `min_size_full()` take the
                    // editor out of content-flow and fill that viewport, so it
                    // never sizes to its own (growing) content. With
                    // `WrapMethod::EditorWidth` the text then wraps to the box
                    // instead of pushing the chat panel wider.
                    //
                    // `ensure_visible` keeps the cursor in view as the user
                    // types — without it the scroll never follows the caret
                    // when the text grows beyond the input box.
                    let input_cursor = input_editor.cursor();
                    let input_editor_for_vis = input_editor.clone();
                    scroll(
                        editor_view(input_editor, debug_breakline, is_active)
                            .style(move |s| {
                                let _ = config.get();
                                s.absolute()
                                    .min_size_full()
                                    .set(WrapProp, WrapMethod::EditorWidth)
                            }),
                    )
                    .ensure_visible(move || {
                        let cursor = input_cursor.get();
                        let offset = cursor.offset();
                        input_editor_for_vis.doc_signal().track();
                        input_editor_for_vis.kind.track();

                        let LineRegion { x, width, rvline } = cursor_caret(
                            &input_editor_for_vis.editor,
                            offset,
                            !cursor.is_insert(),
                            cursor.affinity,
                        );
                        let cfg = config.get_untracked();
                        let line_height = cfg.editor.line_height();
                        let vline = input_editor_for_vis.editor.vline_of_rvline(rvline);
                        let vline = input_editor_for_vis.visual_line(vline.get());
                        Rect::from_origin_size(
                            (x, (vline * line_height) as f64),
                            (width, line_height as f64),
                        )
                        .inflate(10.0, 10.0)
                    })
                    .scroll_style(|s| s.hide_bars(true))
                    .style(|s| s.size_pct(100.0, 100.0))
                })
                    .style(move |s| {
                        let config = config.get();
                        s.width_pct(100.0)
                            .height(input_height.get())
                            .border(1.0)
                            .border_radius(8.0)
                            .border_color(config.color(LapceColor::LAPCE_BORDER))
                            .background(config.color(LapceColor::EDITOR_BACKGROUND))
                            .font_size(13.0)
                            // Internal padding so the caret/text clears the
                            // rounded border (was blinking in the corner).
                            .padding_left(10.0)
                            .padding_right(10.0)
                            .padding_top(8.0)
                            .padding_bottom(8.0)
                    }),
                // Bottom toolbar: agent selector (bottom-left), model selector
                // (to its right), and the Send button pushed to the far right.
                // The agent/model lists open as overlays (below), so these
                // triggers only toggle the shared `dropdown` signal.
                {
                    let agent_name_sig = chat.agent_name;
                    let config_options_sig = chat.config_options;
                    let is_loading_sig = chat.is_loading;
                    stack((
                        // Agent selector (bottom-left).
                        label(move || {
                            format!("agent: {} ▾", agent_name_sig.get())
                        })
                        .on_click_stop(move |_| {
                            let opening =
                                dropdown.get_untracked() != ChatDropdown::Agent;
                            dropdown.set(if opening {
                                ChatDropdown::Agent
                            } else {
                                ChatDropdown::None
                            });
                        })
                        .style(move |s| {
                            let config = config.get();
                            s.padding_horiz(8.0)
                                .padding_vert(4.0)
                                .border(1.0)
                                .border_radius(6.0)
                                .font_size(11.0)
                                .cursor(CursorStyle::Pointer)
                                .selectable(false)
                                .color(
                                    config.color(LapceColor::EDITOR_FOREGROUND),
                                )
                                .border_color(
                                    config.color(LapceColor::LAPCE_BORDER),
                                )
                                .hover(|s| {
                                    s.background(config.color(
                                        LapceColor::PANEL_HOVERED_BACKGROUND,
                                    ))
                                })
                        }),
                        // Model selector (right of the agent). Hidden unless
                        // the agent advertises a model option.
                        label(move || {
                            let sel =
                                parse_model_select(&config_options_sig.get());
                            match sel {
                                Some(m) => {
                                    let name = m
                                        .items
                                        .iter()
                                        .find(|(v, _)| *v == m.current)
                                        .map(|(_, n)| n.clone())
                                        .unwrap_or_else(|| m.current.clone());
                                    format!(
                                        "model: {} ▾",
                                        if name.is_empty() {
                                            "(default)".to_string()
                                        } else {
                                            name
                                        }
                                    )
                                }
                                None => String::new(),
                            }
                        })
                        .on_click_stop(move |_| {
                            let opening =
                                dropdown.get_untracked() != ChatDropdown::Model;
                            dropdown.set(if opening {
                                ChatDropdown::Model
                            } else {
                                ChatDropdown::None
                            });
                        })
                        .style(move |s| {
                            let config = config.get();
                            let hidden =
                                parse_model_select(&config_options_sig.get())
                                    .is_none();
                            s.margin_left(6.0)
                                .padding_horiz(8.0)
                                .padding_vert(4.0)
                                .border(1.0)
                                .border_radius(6.0)
                                .font_size(11.0)
                                .cursor(CursorStyle::Pointer)
                                .selectable(false)
                                .color(
                                    config.color(LapceColor::EDITOR_FOREGROUND),
                                )
                                .border_color(
                                    config.color(LapceColor::LAPCE_BORDER),
                                )
                                .hover(|s| {
                                    s.background(config.color(
                                        LapceColor::PANEL_HOVERED_BACKGROUND,
                                    ))
                                })
                                .apply_if(hidden, |s| s.hide())
                        }),
                        // Spacer pushes the Send button to the far right.
                        empty().style(|s| s.flex_grow(1.0)),
                        // Send button (bottom-right).
                        label(|| "Send".to_string())
                            .on_click_stop(move |_| {
                                chat_for_click.send_prompt();
                            })
                            .style(move |s| {
                                let config = config.get();
                                s.padding_horiz(12.0)
                                    .padding_vert(5.0)
                                    .items_center()
                                    .justify_center()
                                    .border(1.0)
                                    .border_radius(6.0)
                                    .border_color(
                                        config.color(LapceColor::LAPCE_BORDER),
                                    )
                                    .cursor(CursorStyle::Pointer)
                                    .selectable(false)
                                    .apply_if(is_loading_sig.get(), |s| s.hide())
                                    .hover(|s| {
                                        s.background(config.color(
                                            LapceColor::PANEL_HOVERED_BACKGROUND,
                                        ))
                                    })
                            }),
                    ))
                    .style(|s| {
                        s.flex_row()
                            .items_center()
                            .width_pct(100.0)
                            .margin_top(6.0)
                    })
                },
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
        // Dropdown overlay — the agent/model/history lists render here as
        // popups (mirrors lapce's `alert_box` modal pattern) so opening one
        // never pushes the surrounding layout. A single absolute, full-panel
        // layer hosts exactly the active list; clicking outside closes it.
        // History is a centered, scrollable modal (most-recent-first); the
        // agent/model lists open at the bottom-left, rising above the input
        // toolbar.
        {
            let dropdown = dropdown;
            let agent_name_sig = chat.agent_name;
            let config_options_sig = chat.config_options;
            let sessions_sig = chat.sessions;
            let input_height = chat.input_height;
            let agent_names = agent_names.clone();
            let chat_for_agent = chat.clone();
            let chat_for_model = chat.clone();
            let wtd_for_hist = window_tab_data.clone();
            let chat_id = chat.chat_id;

            container(
                dyn_stack(
                    move || match dropdown.get() {
                        ChatDropdown::None => Vec::new(),
                        other => vec![other],
                    },
                    |kind| *kind as u8,
                    move |kind| match kind {
                        ChatDropdown::Agent => {
                            let chat_sel = chat_for_agent.clone();
                            let names = agent_names.clone();
                            scroll(
                                dyn_stack(
                                    move || names.clone(),
                                    |name| name.clone(),
                                    move |name| {
                                        let chat_sel = chat_sel.clone();
                                        let click_name = name.clone();
                                        let style_name = name.clone();
                                        label(move || name.clone())
                                            .on_click_stop(move |_| {
                                                chat_sel.select_agent(
                                                    click_name.clone(),
                                                );
                                                dropdown.set(ChatDropdown::None);
                                            })
                                            .style(move |s| {
                                                let config = config.get();
                                                let active = agent_name_sig.get()
                                                    == style_name;
                                                s.width_pct(100.0)
                                                    .padding_horiz(10.0)
                                                    .padding_vert(4.0)
                                                    .font_size(11.0)
                                                    .cursor(CursorStyle::Pointer)
                                                    .selectable(false)
                                                    .color(config.color(
                                                        LapceColor::EDITOR_FOREGROUND,
                                                    ))
                                                    .background(if active {
                                                        config.color(
                                                            LapceColor::PANEL_HOVERED_BACKGROUND,
                                                        )
                                                    } else {
                                                        Color::TRANSPARENT
                                                    })
                                                    .hover(|s| {
                                                        s.background(config.color(
                                                            LapceColor::PANEL_HOVERED_BACKGROUND,
                                                        ))
                                                    })
                                            })
                                    },
                                )
                                .style(|s| s.flex_col().min_width(160.0)),
                            )
                            .on_event_stop(EventListener::PointerDown, |_| {})
                            .style(move |s| {
                                let config = config.get();
                                s.max_height(260.0)
                                    .margin_left(16.0)
                                    .margin_bottom(input_height.get() + 56.0)
                                    .border(1.0)
                                    .border_radius(6.0)
                                    .border_color(
                                        config.color(LapceColor::LAPCE_BORDER),
                                    )
                                    .background(
                                        config.color(LapceColor::PANEL_BACKGROUND),
                                    )
                            })
                            .into_any()
                        }
                        ChatDropdown::Model => {
                            let chat_sel = chat_for_model.clone();
                            scroll(
                                dyn_stack(
                                    move || {
                                        parse_model_select(&config_options_sig.get())
                                            .map(|m| {
                                                let cid = m.config_id;
                                                let cur = m.current;
                                                m.items
                                                    .into_iter()
                                                    .map(|(v, n)| {
                                                        (
                                                            cid.clone(),
                                                            cur.clone(),
                                                            v,
                                                            n,
                                                        )
                                                    })
                                                    .collect::<Vec<_>>()
                                            })
                                            .unwrap_or_default()
                                    },
                                    |row| row.2.clone(),
                                    move |row| {
                                        let chat_sel = chat_sel.clone();
                                        let (cid, cur, value, name) = row.clone();
                                        let style_cur = cur.clone();
                                        let style_value = value.clone();
                                        label(move || name.clone())
                                            .on_click_stop(move |_| {
                                                chat_sel.select_config_option(
                                                    cid.clone(),
                                                    value.clone(),
                                                );
                                                dropdown.set(ChatDropdown::None);
                                            })
                                            .style(move |s| {
                                                let config = config.get();
                                                let active =
                                                    style_value == style_cur;
                                                s.width_pct(100.0)
                                                    .padding_horiz(10.0)
                                                    .padding_vert(4.0)
                                                    .font_size(11.0)
                                                    .cursor(CursorStyle::Pointer)
                                                    .selectable(false)
                                                    .color(config.color(
                                                        LapceColor::EDITOR_FOREGROUND,
                                                    ))
                                                    .background(if active {
                                                        config.color(
                                                            LapceColor::PANEL_HOVERED_BACKGROUND,
                                                        )
                                                    } else {
                                                        Color::TRANSPARENT
                                                    })
                                                    .hover(|s| {
                                                        s.background(config.color(
                                                            LapceColor::PANEL_HOVERED_BACKGROUND,
                                                        ))
                                                    })
                                            })
                                    },
                                )
                                .style(|s| s.flex_col().min_width(180.0)),
                            )
                            .on_event_stop(EventListener::PointerDown, |_| {})
                            .style(move |s| {
                                let config = config.get();
                                s.max_height(260.0)
                                    .margin_left(16.0)
                                    .margin_bottom(input_height.get() + 56.0)
                                    .border(1.0)
                                    .border_radius(6.0)
                                    .border_color(
                                        config.color(LapceColor::LAPCE_BORDER),
                                    )
                                    .background(
                                        config.color(LapceColor::PANEL_BACKGROUND),
                                    )
                            })
                            .into_any()
                        }
                        ChatDropdown::History => {
                            let wtd = wtd_for_hist.clone();
                            scroll(
                                dyn_stack(
                                    move || sessions_sig.get(),
                                    |row| row.id.clone(),
                                    move |row| {
                                        let wtd = wtd.clone();
                                        let click_id = row.id.clone();
                                        // Copy crow-ade's setSessions layout:
                                        // line 1 = full agent_id + date;
                                        // line 2 = title / first message.
                                        // agent_id includes the 1-based index
                                        // suffix (e.g. "cool-name-1"); fall
                                        // back to session_id if absent.
                                        let full_id = row
                                            .agent_id
                                            .clone()
                                            .unwrap_or_else(|| row.id.clone());
                                        let has_date = row.updated_at.is_some();
                                        let date_text = row
                                            .updated_at
                                            .as_deref()
                                            .map(|d| {
                                                if d.len() >= 10 {
                                                    d[..10].to_string()
                                                } else {
                                                    d.to_string()
                                                }
                                            })
                                            .unwrap_or_default();
                                        let has_title = row
                                            .title
                                            .as_deref()
                                            .map(|t| {
                                                !t.is_empty()
                                                    && t != "Untitled Chat"
                                            })
                                            .unwrap_or(false);
                                        let title_text = row
                                            .title
                                            .as_deref()
                                            .map(|t| {
                                                t.replace(['\n', '\r'], " ")
                                                    .split_whitespace()
                                                    .collect::<Vec<_>>()
                                                    .join(" ")
                                            })
                                            .unwrap_or_default();
                                        stack((
                                            // Line 1: full id + date
                                            stack((
                                                label(move || full_id.clone())
                                                    .style(move |s| {
                                                        let config = config.get();
                                                        s.flex_grow(1.0)
                                                            .flex_basis(0.0)
                                                            .font_size(11.0)
                                                            .selectable(false)
                                                            .color(config.color(
                                                                LapceColor::EDITOR_FOREGROUND,
                                                            ))
                                                    }),
                                                label(move || date_text.clone())
                                                    .style(move |s| {
                                                        let config = config.get();
                                                        s.margin_left(8.0)
                                                            .font_size(10.0)
                                                            .selectable(false)
                                                            .color(config.color(
                                                                LapceColor::EDITOR_DIM,
                                                            ))
                                                            .apply_if(
                                                                !has_date,
                                                                |s| s.hide(),
                                                            )
                                                    }),
                                            ))
                                            .style(|s| {
                                                s.flex_row()
                                                    .items_center()
                                                    .width_pct(100.0)
                                            }),
                                            // Line 2: title / first message
                                            label(move || title_text.clone())
                                                .style(move |s| {
                                                    let config = config.get();
                                                    s.width_pct(100.0)
                                                        .font_size(10.0)
                                                        .selectable(false)
                                                        .color(config.color(
                                                            LapceColor::EDITOR_DIM,
                                                        ))
                                                        .apply_if(
                                                            !has_title,
                                                            |s| s.hide(),
                                                        )
                                                }),
                                        ))
                                        .on_click_stop(move |_| {
                                            wtd.load_chat_session(
                                                chat_id,
                                                click_id.clone(),
                                            );
                                            dropdown.set(ChatDropdown::None);
                                        })
                                        .style(move |s| {
                                            let config = config.get();
                                            s.flex_col()
                                                .width_pct(100.0)
                                                .padding_horiz(10.0)
                                                .padding_vert(4.0)
                                                .cursor(CursorStyle::Pointer)
                                                .hover(|s| {
                                                    s.background(config.color(
                                                        LapceColor::PANEL_HOVERED_BACKGROUND,
                                                    ))
                                                })
                                        })
                                    },
                                )
                                .style(|s| s.flex_col().width_pct(100.0)),
                            )
                            .on_event_stop(EventListener::PointerDown, |_| {})
                            .style(move |s| {
                                let config = config.get();
                                // Fixed-pixel cap (NOT a percentage — the
                                // overlay's inner stack is auto-height, so a
                                // %-cap resolved to nothing and the 500+ row
                                // list filled the whole panel).
                                s.width_pct(90.0)
                                    .min_width(300.0)
                                    .max_height(320.0)
                                    .border(1.0)
                                    .border_radius(8.0)
                                    .border_color(
                                        config.color(LapceColor::LAPCE_BORDER),
                                    )
                                    .background(
                                        config.color(LapceColor::PANEL_BACKGROUND),
                                    )
                            })
                            .into_any()
                        }
                        ChatDropdown::None => empty().into_any(),
                    },
                )
                .style(|s| s.flex_col()),
            )
            .on_event_stop(EventListener::PointerDown, move |_| {
                dropdown.set(ChatDropdown::None);
            })
            .style(move |s| {
                let config = config.get();
                let kind = dropdown.get();
                s.absolute()
                    .size_pct(100.0, 100.0)
                    .flex_col()
                    .apply_if(kind == ChatDropdown::None, |s| s.hide())
                    .apply_if(kind == ChatDropdown::History, |s| {
                        s.items_center().justify_center().background(
                            config
                                .color(LapceColor::LAPCE_DROPDOWN_SHADOW)
                                .multiply_alpha(0.3),
                        )
                    })
                    .apply_if(
                        kind == ChatDropdown::Agent || kind == ChatDropdown::Model,
                        |s| s.items_start().justify_end(),
                    )
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
) -> AnyView {
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

    // Read tools keep the tool-fixture card (border + header with the
    // clickable path link) but render NO body — the file content never
    // dumps into the chat. The header itself opens the file on click.

    // Header text: the file path when we have one (rendered as a clickable
    // link), else the title with any read:/write:/edit: prefix stripped.
    let header_text = match &file_path {
        Some(p) => p.clone(),
        None => strip_tool_prefix(&title),
    };
    let internal_command = chat.common.internal_command;
    let path_for_click = file_path.clone();
    let path_for_header = file_path.clone();
    let internal_command_header = chat.common.internal_command;

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
                        // Read tools have no body to collapse — hide the caret.
                        .apply_if(is_read, |s| s.hide())
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
                if is_read {
                    // Read fixture has no body — the whole header opens the file.
                    if let Some(p) = &path_for_header {
                        internal_command_header.send(InternalCommand::OpenFile {
                            path: PathBuf::from(p),
                        });
                    }
                } else {
                    open.update(|o| *o = !*o);
                }
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
    .into_any()
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
