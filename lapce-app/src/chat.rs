use std::cell::Cell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, RwLock};

use floem::keyboard::Modifiers;
use floem::reactive::{RwSignal, Scope, SignalGet, SignalUpdate, SignalWith};
use floem::views::editor::command::CommandExecuted;
use lapce_core::buffer::diff::{DiffLines, rope_diff};
use lapce_core::command::EditCommand;
use lapce_core::mode::Mode;
use lapce_core::syntax::Syntax;
use lapce_rpc::proxy::ProxyNotification;
use lapce_xi_rope::Rope;

use crate::chat_terminal::{ChatRawTerminal, ChatTermHandle};
use crate::command::{CommandKind, LapceCommand};
use crate::editor::EditorData;
use crate::id::ChatId;
use crate::keypress::{KeyPressFocus, condition::Condition};
use crate::main_split::Editors;
use crate::window_tab::CommonData;

/// Status of a tool call.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl ToolStatus {
    pub fn icon(&self) -> &'static str {
        match self {
            ToolStatus::Pending => "○",
            ToolStatus::InProgress => "◐",
            ToolStatus::Completed => "●",
            ToolStatus::Failed => "✕",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "in_progress" => ToolStatus::InProgress,
            "completed" => ToolStatus::Completed,
            "failed" => ToolStatus::Failed,
            _ => ToolStatus::Pending,
        }
    }
}

/// A single display block in the chat panel.
///
/// Every variant carries a stable `id` so the view's keyed list can keep
/// per-block state (text/collapse signals) mounted across streaming updates
/// instead of tearing it down on every chunk.
#[derive(Clone)]
pub enum ChatBlock {
    /// User-sent text message (immutable after creation).
    UserText { id: u64, text: String },
    /// Agent response, rendered as markdown. The view re-parses `text` on
    /// every streamed chunk (pulldown_cmark, same renderer as hover + the
    /// plugin README page).
    AssistantText {
        id: u64,
        text: RwSignal<String>,
    },
    /// Agent internal reasoning / thinking (collapsible).
    Thinking {
        id: u64,
        text: RwSignal<String>,
        open: RwSignal<bool>,
    },
    /// A tool call with status tracking. `id` is a stable internal id for
    /// view diffing; `tool_id` is the agent's `toolCallId` used to match
    /// `tool_call_update` notifications.
    ToolCall {
        id: u64,
        tool_id: String,
        title: String,
        kind: String,
        status: ToolStatus,
        raw_input: Option<String>,
        raw_output: Option<String>,
        terminal_id: Option<String>,
        /// File path from a `diff` content block (edit/write tools).
        diff_path: Option<String>,
        /// Original content for an edit (`None` for a write / new file).
        old_text: Option<String>,
        /// New content from a `diff` content block (edit/write tools).
        new_text: Option<String>,
    },
    /// System / informational message.
    System { id: u64, text: String },
}

impl ChatBlock {
    pub fn id(&self) -> u64 {
        match self {
            ChatBlock::UserText { id, .. }
            | ChatBlock::AssistantText { id, .. }
            | ChatBlock::Thinking { id, .. }
            | ChatBlock::ToolCall { id, .. }
            | ChatBlock::System { id, .. } => *id,
        }
    }
}

/// Data model for the ACP chat panel.
#[derive(Clone)]
pub struct ChatData {
    /// Stable identity for this chat instance. Echoed to the proxy in
    /// `AcpCreateSession` and back in `AcpSessionCreated`/`Failed` so the
    /// right chat claims its session when several run at once.
    pub chat_id: ChatId,
    /// Ordered list of display blocks.
    pub blocks: RwSignal<Vec<ChatBlock>>,
    /// Current ACP session ID.
    pub session_id: RwSignal<Option<String>>,
    /// Whether the agent is currently processing a prompt.
    pub is_loading: RwSignal<bool>,
    /// The chat input, a real (multi-line) lapce editor backed by an
    /// in-memory local doc. Enter sends / Shift+Enter inserts a newline
    /// (handled by `ChatInputFocus` + the `Focus::Panel(Chat)` key route).
    pub input_editor: EditorData,
    /// Height (px) of the input editor. Drag-resizable via the handle
    /// above it; defaults to ~10 lines.
    pub input_height: RwSignal<f64>,
    /// Monotonically increasing counter, bumped on every block mutation.
    /// The view watches this to trigger auto-scroll.
    pub scroll_version: RwSignal<u64>,
    /// Shared monotonic block-id allocator (interior mutability so clones
    /// of `ChatData` share the same counter).
    next_id: Rc<Cell<u64>>,
    /// Long-lived scope (the window-tab scope). Every reactive signal the
    /// chat creates — block text/collapse signals — MUST be allocated here,
    /// never in a callback/effect scope, or floem will dispose them on the
    /// next effect re-run and the next paint panics on a dead `.get()`.
    scope: Scope,
    /// A prompt the user typed before the ACP session existed; drained and
    /// sent once `handle_session_created` fires so the first message isn't
    /// swallowed by session init.
    pending_prompt: RwSignal<Option<String>>,
    /// One-shot guard so eager connect only requests a session once.
    session_requested: RwSignal<bool>,
    /// Inline terminal grids for ACP client-side terminals, keyed by the
    /// string terminalId the agent receives from terminal/create.
    pub terminals: Rc<std::cell::RefCell<HashMap<String, ChatTermHandle>>>,
    pub common: Rc<CommonData>,
}

/// Key focus for the chat input editor. Wraps the input `EditorData` so it
/// types like a normal editor, but intercepts a bare **Enter** to send the
/// prompt (Shift+Enter still inserts a newline). `get_mode` always returns
/// `Mode::Insert` so keys insert text regardless of the global modal setting.
#[derive(Clone)]
pub struct ChatInputFocus {
    pub editor: EditorData,
    pub chat: ChatData,
}

impl std::fmt::Debug for ChatInputFocus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatInputFocus").finish_non_exhaustive()
    }
}

impl KeyPressFocus for ChatInputFocus {
    fn get_mode(&self) -> Mode {
        Mode::Insert
    }

    fn check_condition(&self, condition: Condition) -> bool {
        self.editor.check_condition(condition)
    }

    fn run_command(
        &self,
        command: &LapceCommand,
        count: Option<usize>,
        mods: Modifiers,
    ) -> CommandExecuted {
        if let CommandKind::Edit(EditCommand::InsertNewLine) = &command.kind {
            if !mods.contains(Modifiers::SHIFT) {
                self.chat.send_prompt();
                return CommandExecuted::Yes;
            }
        }
        self.editor.run_command(command, count, mods)
    }

    fn receive_char(&self, c: &str) {
        self.editor.receive_char(c);
    }
}

/// Extract the terminalId from a `tool_call_update`'s `content` array, if it
/// carries a terminal block `{ "type": "terminal", "terminalId": "..." }`.
///
/// Pure and allocation-light so it can be unit-tested against the exact wire
/// JSON an ACP agent sends (see crow-cli `tools.py` / the ACP schema
/// `Terminal` type) without constructing a full `ChatData`.
pub fn extract_terminal_id(update: &serde_json::Value) -> Option<String> {
    update
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| {
            arr.iter().find(|b| {
                b.get("type").and_then(|t| t.as_str()) == Some("terminal")
            })
        })
        .and_then(|b| {
            b.get("terminalId")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
}

/// Extract concatenated text from a `tool_call` / `tool_call_update`
/// `content` array.
///
/// crow-cli's `execute` tool runs its OWN internal PTY and returns the
/// captured ANSI output as text content blocks rather than using the ACP
/// `terminal/create` + `Terminal`-block protocol. The completed
/// `tool_call_update` looks like (real wire capture):
///
/// ```json
/// { "content": [ { "type": "content",
///                  "content": { "type": "text", "text": "\u001b[?2004l\r..." } } ],
///   "status": "completed" }
/// ```
///
/// We accept both the wrapped `{type:"content", content:{type:"text",text}}`
/// form and a bare `{type:"text", text}` form, concatenating all text blocks.
/// Returns `None` if there is no text.
pub fn extract_content_text(update: &serde_json::Value) -> Option<String> {
    let arr = update.get("content").and_then(|c| c.as_array())?;
    let mut out = String::new();
    for block in arr {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("content") => {
                // {type:"content", content:{type:"text", text:"..."}}
                if let Some(text) = block
                    .get("content")
                    .and_then(|c| c.get("text"))
                    .and_then(|t| t.as_str())
                {
                    out.push_str(text);
                }
            }
            Some("text") => {
                if let Some(text) =
                    block.get("text").and_then(|t| t.as_str())
                {
                    out.push_str(text);
                }
            }
            _ => {}
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// A parsed ACP `diff` content block — what the edit/write tools send.
/// Wire shape (ACP `Diff` type, camelCase field aliases):
/// `{ "type": "diff", "path": "...", "newText": "...", "oldText": "..." }`.
/// `old_text` is `None` for a brand-new file (the write tool omits it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolDiff {
    pub path: String,
    pub old_text: Option<String>,
    pub new_text: String,
}

/// Extract the first `diff` content block from a `tool_call` /
/// `tool_call_update` `content` array, if present. crow-cli's edit/write
/// tools emit this via `acp.helpers.tool_diff_content` (edit carries
/// `oldText`; write omits it → new-file). Pure so it can be unit-tested
/// against the exact wire JSON without constructing a full `ChatData`.
pub fn extract_diff(update: &serde_json::Value) -> Option<ToolDiff> {
    let arr = update.get("content").and_then(|c| c.as_array())?;
    let block = arr
        .iter()
        .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("diff"))?;
    let path = block
        .get("path")
        .and_then(|p| p.as_str())
        .map(String::from)
        .unwrap_or_default();
    // ACP serializes by alias (camelCase); accept snake_case too (crow-ade
    // does the same `newText ?? new_text`).
    let new_text = block
        .get("newText")
        .or_else(|| block.get("new_text"))
        .and_then(|t| t.as_str())
        .map(String::from)
        .unwrap_or_default();
    let old_text = block
        .get("oldText")
        .or_else(|| block.get("old_text"))
        .and_then(|t| t.as_str())
        .map(String::from);
    Some(ToolDiff {
        path,
        old_text,
        new_text,
    })
}

/// The kind of a single rendered diff line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatDiffLineKind {
    /// Unchanged context line.
    Context,
    /// Line present only in the new content (green).
    Added,
    /// Line present only in the old content (red).
    Removed,
}

/// One line of an inline diff rendered in a chat tool fixture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatDiffLine {
    pub kind: ChatDiffLineKind,
    pub text: String,
}

/// Compute a line-based diff between `old` and `new`, returning a flat list
/// of context/added/removed lines with ~3 lines of context around each
/// change (collapsed regions become a single `⋯` marker). Built on lapce's
/// own `rope_diff` (the same LCS the diff editor uses).
///
/// For a write/new-file, pass `old = ""` → every line comes back `Added`.
/// Pure so it can be unit-tested without a `ChatData`.
pub fn diff_lines(old: &str, new: &str) -> Vec<ChatDiffLine> {
    use ChatDiffLineKind::*;

    let old_rope = Rope::from(old);
    let new_rope = Rope::from(new);
    // rev/atomic_rev are the cancellation channel for async diffs; a fixed
    // matching pair (0, 0) means "never cancel" for this one-shot compute.
    let atomic_rev = Arc::new(AtomicU64::new(0));
    let Some(changes) = rope_diff(old_rope, new_rope, 0, atomic_rev, Some(3))
    else {
        return Vec::new();
    };

    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let mut out = Vec::new();
    for change in changes {
        match change {
            DiffLines::Left(range) => {
                for i in range {
                    if let Some(l) = old_lines.get(i) {
                        out.push(ChatDiffLine {
                            kind: Removed,
                            text: (*l).to_string(),
                        });
                    }
                }
            }
            DiffLines::Right(range) => {
                for i in range {
                    if let Some(l) = new_lines.get(i) {
                        out.push(ChatDiffLine {
                            kind: Added,
                            text: (*l).to_string(),
                        });
                    }
                }
            }
            DiffLines::Both(info) => {
                let len = info.right.len();
                let skip = info.skip.clone();
                let mut emitted_marker = false;
                for offset in 0..len {
                    let is_skipped = skip
                        .as_ref()
                        .map(|s| s.contains(&offset))
                        .unwrap_or(false);
                    if is_skipped {
                        if !emitted_marker {
                            out.push(ChatDiffLine {
                                kind: Context,
                                text: "⋯".to_string(),
                            });
                            emitted_marker = true;
                        }
                        continue;
                    }
                    let i = info.right.start + offset;
                    if let Some(l) = new_lines.get(i) {
                        out.push(ChatDiffLine {
                            kind: Context,
                            text: (*l).to_string(),
                        });
                    }
                }
            }
        }
    }
    out
}

/// Extract a file path associated with a tool call, for rendering a clickable
/// "open file" link. Prefers the diff block's `path` (edit/write carry it),
/// else falls back to the tool's `rawInput` args (`file_path`, then `path`).
/// crow-cli's read/write/edit tools all pass `file_path`. Pure & testable.
pub fn extract_file_path(
    diff_path: Option<&str>,
    raw_input: Option<&str>,
) -> Option<String> {
    if let Some(p) = diff_path.map(str::trim).filter(|p| !p.is_empty()) {
        return Some(p.to_string());
    }
    let raw = raw_input?;
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    for key in ["file_path", "path"] {
        if let Some(p) = value.get(key).and_then(|v| v.as_str()) {
            if !p.trim().is_empty() {
                return Some(p.to_string());
            }
        }
    }
    None
}

impl ChatData {
    pub fn new(cx: Scope, editors: Editors, common: Rc<CommonData>) -> Self {
        Self::new_with_id(cx, editors, common, ChatId::next())
    }

    /// Build a chat with a specific id. Used when an `EditorTabChild::Chat(id)`
    /// already exists (e.g. workspace restore) and the `ChatData` must line
    /// up with that id for ACP routing.
    pub fn new_with_id(
        cx: Scope,
        editors: Editors,
        common: Rc<CommonData>,
        chat_id: ChatId,
    ) -> Self {
        // The input is a real editor backed by an in-memory local doc
        // (same mechanism as the find/replace editors).
        let input_editor = editors.make_local(cx, common.clone());
        // Markdown syntax so fenced code blocks typed into the input are
        // highlighted (same mechanism as opening a .md file).
        {
            let doc = input_editor.doc();
            doc.set_syntax(Syntax::init(Path::new("chat.md")));
            doc.trigger_syntax_change(None);
        }
        Self {
            chat_id,
            blocks: cx.create_rw_signal(Vec::new()),
            session_id: cx.create_rw_signal(None),
            is_loading: cx.create_rw_signal(false),
            input_editor,
            input_height: cx.create_rw_signal(200.0),
            scroll_version: cx.create_rw_signal(0),
            next_id: Rc::new(Cell::new(1)),
            scope: cx,
            pending_prompt: cx.create_rw_signal(None),
            session_requested: cx.create_rw_signal(false),
            terminals: Rc::new(std::cell::RefCell::new(HashMap::new())),
            common,
        }
    }

    /// Request an ACP session exactly once. Called eagerly on window build
    /// so the session is ready before the user types (no "send to init").
    pub fn ensure_session(&self) {
        if self.session_requested.get_untracked() {
            return;
        }
        if self.session_id.get_untracked().is_some() {
            return;
        }
        self.session_requested.set(true);
        self.common
            .proxy
            .notification(ProxyNotification::AcpCreateSession {
                agent_name: "crow-cli".to_string(),
                command: "crow-cli".to_string(),
                args: vec!["acp".to_string()],
                env: vec![],
                cwd: self
                    .common
                    .workspace
                    .path
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
                chat_id: self.chat_id.to_raw(),
            });
    }

    fn new_id(&self) -> u64 {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        id
    }

    /// Bump the scroll version so the view auto-scrolls.
    fn bump_scroll(&self) {
        self.scroll_version.update(|v| *v += 1);
    }

    /// Push a new block to the chat log.
    pub fn push_block(&self, block: ChatBlock) {
        self.blocks.update(|blocks| blocks.push(block));
        self.bump_scroll();
    }

    /// Convenience constructors (assign a fresh id).
    fn user_block(&self, text: String) -> ChatBlock {
        ChatBlock::UserText { id: self.new_id(), text }
    }
    fn system_block(&self, text: String) -> ChatBlock {
        ChatBlock::System { id: self.new_id(), text }
    }

    /// Append text to the last AssistantText block, or create a new one.
    /// The view re-parses `text` as markdown on every chunk, so the model
    /// only needs to accumulate the source string here.
    pub fn append_assistant_text(&self, text: &str) {
        self.blocks.update(|blocks| {
            if let Some(ChatBlock::AssistantText { text: t, .. }) =
                blocks.last_mut()
            {
                t.update(|s| s.push_str(text));
            } else {
                // Allocate on the long-lived scope (see `scope` field doc).
                let t = self.scope.create_rw_signal(text.to_string());
                blocks.push(ChatBlock::AssistantText {
                    id: self.new_id(),
                    text: t,
                });
            }
        });
        self.bump_scroll();
    }

    /// Append text to the last Thinking block, or create a new one.
    pub fn append_thinking_text(&self, text: &str) {
        self.blocks.update(|blocks| {
            if let Some(ChatBlock::Thinking { text: t, .. }) = blocks.last_mut() {
                t.update(|s| s.push_str(text));
            } else {
                let t = self.scope.create_rw_signal(text.to_string());
                // Visible by default — thinking tokens must actually show.
                let open = self.scope.create_rw_signal(true);
                blocks.push(ChatBlock::Thinking {
                    id: self.new_id(),
                    text: t,
                    open,
                });
            }
        });
        self.bump_scroll();
    }

    /// Add or update a tool call block.
    pub fn upsert_tool_call(
        &self,
        id: &str,
        title: &str,
        kind: &str,
        status: ToolStatus,
        raw_input: Option<String>,
        raw_output: Option<String>,
    ) {
        self.blocks.update(|blocks| {
            for block in blocks.iter_mut() {
                if let ChatBlock::ToolCall {
                    tool_id,
                    title: existing_title,
                    kind: existing_kind,
                    status: existing_status,
                    raw_input: existing_input,
                    raw_output: existing_output,
                    ..
                } = block
                {
                    if tool_id == id {
                        if !title.is_empty() {
                            *existing_title = title.to_string();
                        }
                        if !kind.is_empty() {
                            *existing_kind = kind.to_string();
                        }
                        *existing_status = status;
                        if raw_input.is_some() {
                            *existing_input = raw_input;
                        }
                        if raw_output.is_some() {
                            *existing_output = raw_output;
                        }
                        return;
                    }
                }
            }
            blocks.push(ChatBlock::ToolCall {
                id: self.new_id(),
                tool_id: id.to_string(),
                title: title.to_string(),
                kind: kind.to_string(),
                status,
                raw_input,
                raw_output,
                terminal_id: None,
                diff_path: None,
                old_text: None,
                new_text: None,
            });
        });
        self.bump_scroll();
    }

    /// Update an existing tool call's status/content by ID.
    pub fn update_tool_call(
        &self,
        id: &str,
        title: Option<&str>,
        kind: Option<&str>,
        status: Option<ToolStatus>,
        raw_input: Option<String>,
        raw_output: Option<String>,
    ) {
        self.blocks.update(|blocks| {
            for block in blocks.iter_mut() {
                if let ChatBlock::ToolCall {
                    tool_id,
                    title: existing_title,
                    kind: existing_kind,
                    status: existing_status,
                    raw_input: existing_input,
                    raw_output: existing_output,
                    ..
                } = block
                {
                    if tool_id == id {
                        if let Some(t) = title {
                            *existing_title = t.to_string();
                        }
                        if let Some(k) = kind {
                            *existing_kind = k.to_string();
                        }
                        if let Some(s) = status {
                            *existing_status = s;
                        }
                        if raw_input.is_some() {
                            *existing_input = raw_input;
                        }
                        if raw_output.is_some() {
                            *existing_output = raw_output;
                        }
                        return;
                    }
                }
            }
        });
        self.bump_scroll();
    }

    /// Send the current input as a prompt to the ACP agent.
    pub fn send_prompt(&self) {
        let text = self
            .input_editor
            .doc()
            .buffer
            .with(|b| b.to_string())
            .trim()
            .to_string();
        if text.is_empty() {
            return;
        }

        // Instant feedback, exactly like the reference store: show the user
        // message immediately, before any round-trip.
        self.push_block(self.user_block(text.clone()));
        // Clear the input editor.
        self.input_editor.doc().reload(Rope::from(""), true);
        self.is_loading.set(true);

        if let Some(session_id) = self.session_id.get_untracked() {
            self.common.proxy.notification(ProxyNotification::AcpPrompt {
                session_id,
                content: text,
            });
        } else {
            // No session yet: queue the prompt and make sure a session is
            // being created. `handle_session_created` will drain this.
            self.pending_prompt.set(Some(text));
            self.ensure_session();
        }
    }

    /// Cancel the current in-progress prompt.
    pub fn cancel_prompt(&self) {
        let session_id = self.session_id.get_untracked();
        if let Some(session_id) = session_id {
            self.common
                .proxy
                .notification(ProxyNotification::AcpCancel { session_id });
        }
        self.is_loading.set(false);
        self.push_block(self.system_block("Cancelled.".to_string()));
    }

    /// Respond to a permission request (allow or deny a tool call).
    /// Handle AcpSessionCreated.
    pub fn handle_session_created(&self, session_id: String) {
        self.session_id.set(Some(session_id.clone()));
        // Drain a prompt the user typed before the session was ready.
        if let Some(text) = self.pending_prompt.get_untracked() {
            self.pending_prompt.set(None);
            self.common.proxy.notification(ProxyNotification::AcpPrompt {
                session_id,
                content: text,
            });
            // is_loading already true from send_prompt.
        } else {
            self.is_loading.set(false);
        }
    }

    /// Handle AcpSessionFailed.
    pub fn handle_session_failed(&self, error: String) {
        self.push_block(self.system_block(format!("Session failed: {}", error)));
        self.is_loading.set(false);
    }

    /// Handle the end of an agent turn. The proxy synthesizes a
    /// `prompt_complete` update (mirroring crow-acp) which routes here.
    pub fn handle_turn_complete(&self) {
        self.is_loading.set(false);
    }

    /// Handle a session/update notification from the proxy.
    pub fn handle_session_update(&self, update: serde_json::Value) {
        // The proxy already normalizes to the inner `SessionUpdate`, but stay
        // tolerant of a full `{ sessionId, update }` envelope just in case.
        let update = update.get("update").cloned().unwrap_or(update);

        let session_update = update
            .get("sessionUpdate")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match session_update {
            "agent_message_chunk" => {
                if let Some(text) = Self::extract_text_from_chunk(&update) {
                    self.append_assistant_text(&text);
                }
            }
            "agent_thought_chunk" => {
                if let Some(text) = Self::extract_text_from_chunk(&update) {
                    self.append_thinking_text(&text);
                }
            }
            // Synthesized by the proxy from the `session/prompt` response
            // (see crow-acp `broadcast_prompt_state`). This is the turn-end
            // signal: clear the loading indicator and freeze the Typst tail.
            "prompt_complete" => {
                self.handle_turn_complete();
            }
            // Synthesized prompt lifecycle marker; `running`/`idle` mirror the
            // loading state. `send_prompt` already sets loading=true, so this
            // is mostly belt-and-suspenders.
            "prompt_state" => {
                let status =
                    update.get("status").and_then(|v| v.as_str()).unwrap_or("");
                match status {
                    "running" => self.is_loading.set(true),
                    "idle" => {
                        self.is_loading.set(false);
                    }
                    _ => {}
                }
            }
            "tool_call" => {
                let id = update
                    .get("toolCallId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let title = update
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let kind = update
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("other")
                    .to_string();
                let status = update
                    .get("status")
                    .and_then(|v| v.as_str())
                    .map(ToolStatus::from_str)
                    .unwrap_or(ToolStatus::InProgress);
                let raw_input = update.get("rawInput").map(|v| {
                    serde_json::to_string_pretty(v).unwrap_or_default()
                });
                let raw_output = update.get("rawOutput").map(|v| {
                    serde_json::to_string_pretty(v).unwrap_or_default()
                });

                self.upsert_tool_call(
                    &id, &title, &kind, status, raw_input, raw_output,
                );

                // edit/write tools can carry their `diff` block on the initial
                // tool_call; attach it so we render a diff, not a plain call.
                if let Some(diff) = extract_diff(&update) {
                    self.attach_diff_to_tool_call(&id, &diff);
                }
            }
            "tool_call_update" => {
                let id = update
                    .get("toolCallId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let title = update.get("title").and_then(|v| v.as_str());
                let kind = update.get("kind").and_then(|v| v.as_str());
                let status = update
                    .get("status")
                    .and_then(|v| v.as_str())
                    .map(ToolStatus::from_str);
                let raw_input = update.get("rawInput").map(|v| {
                    serde_json::to_string_pretty(v).unwrap_or_default()
                });
                let raw_output = update.get("rawOutput").map(|v| {
                    serde_json::to_string_pretty(v).unwrap_or_default()
                });

                // Extract terminal content blocks: {type:"terminal", terminalId:"..."}
                let terminal_id = extract_terminal_id(&update);
                // Extract text content blocks. crow-cli's `execute` tool runs
                // its own PTY and returns the ANSI output here (NOT via a
                // terminal block), so this is the path that actually surfaces
                // command output in the chat.
                let content_text = extract_content_text(&update);
                // Extract a `diff` content block (edit/write tools).
                let diff = extract_diff(&update);

                self.update_tool_call(
                    &id, title, kind, status, raw_input, raw_output,
                );

                tracing::info!(
                    tool_call_id = %id,
                    terminal_id = ?terminal_id,
                    content_text_len = content_text.as_ref().map(|s| s.len()).unwrap_or(0),
                    has_diff = diff.is_some(),
                    "chat: tool_call_update parsed"
                );
                // Terminal-block path (agents using ACP terminal/create).
                if let Some(tid) = terminal_id {
                    self.attach_terminal_to_tool_call(&id, &tid);
                }
                // Text-content path (crow-cli execute): feed the ANSI output
                // into a per-tool grid and render it as an inline terminal.
                if let Some(text) = content_text {
                    self.feed_tool_output_text(&id, &text);
                }
                // Diff path (crow-cli edit/write): render a diff view.
                if let Some(diff) = diff {
                    self.attach_diff_to_tool_call(&id, &diff);
                }
            }
            // Everything else (available_commands_update, current_mode_update,
            // session_info_update, usage_update, plan, ...) is intentionally
            // ignored — we only render what we know how to.
            _ => {}
        }
    }

    /// Extract text from a ContentChunk's `content` field.
    fn extract_text_from_chunk(update: &serde_json::Value) -> Option<String> {
        let content = update.get("content")?;
        let content_type = content.get("type").and_then(|t| t.as_str());
        if content_type == Some("text") {
            return content
                .get("text")
                .and_then(|t| t.as_str())
                .map(String::from);
        }
        content.get("text").and_then(|t| t.as_str()).map(String::from)
    }

    /// Attach a terminal ID to a tool call block and ensure a grid exists.
    fn attach_terminal_to_tool_call(&self, tool_id: &str, terminal_id: &str) {
        tracing::info!(
            tool_id = %tool_id,
            terminal_id = %terminal_id,
            "chat: attaching terminal to tool call"
        );
        // Ensure a ChatRawTerminal grid + repaint signal exist for this
        // terminal. The gen signal is allocated on the long-lived scope so it
        // outlives any single view build.
        {
            let mut terminals = self.terminals.borrow_mut();
            terminals
                .entry(terminal_id.to_string())
                .or_insert_with(|| ChatTermHandle {
                    raw: Arc::new(RwLock::new(ChatRawTerminal::new(24, 80))),
                    paint_gen: self.scope.create_rw_signal(0u64),
                });
        }
        // Set the terminal_id on the matching tool call block AND bump its
        // id so dyn_stack rebuilds the view (the view was first built from
        // the `tool_call` start, before any terminal existed, and a stable
        // key would otherwise leave it stuck on the empty fallback). The
        // grid lives in the map above, so the rebuild loses no output.
        self.blocks.update(|blocks| {
            for block in blocks.iter_mut() {
                if let ChatBlock::ToolCall {
                    id,
                    tool_id: tid,
                    terminal_id: term,
                    ..
                } = block
                {
                    if tid == tool_id {
                        *term = Some(terminal_id.to_string());
                        *id = self.new_id();
                        return;
                    }
                }
            }
        });
        self.bump_scroll();
    }

    /// Feed text output returned inline in a `tool_call_update` `content`
    /// block (crow-cli's `execute` tool returns its captured PTY output this
    /// way instead of using `terminal/create`). The text is fed through a
    /// per-tool alacritty grid keyed by the tool-call id, and the block is
    /// pointed at that grid so the inline terminal renders the ANSI output.
    fn feed_tool_output_text(&self, tool_id: &str, text: &str) {
        tracing::info!(
            tool_id = %tool_id,
            bytes = text.len(),
            "chat: feeding tool output text into inline terminal"
        );
        // Grid keyed by the tool-call id (crow-cli gives us no terminalId).
        let handle = {
            let mut terminals = self.terminals.borrow_mut();
            terminals
                .entry(tool_id.to_string())
                .or_insert_with(|| ChatTermHandle {
                    raw: Arc::new(RwLock::new(ChatRawTerminal::new(24, 80))),
                    paint_gen: self.scope.create_rw_signal(0u64),
                })
                .clone()
        };
        if let Ok(mut grid) = handle.raw.write() {
            grid.feed(text.as_bytes());
        }
        handle.paint_gen.update(|g| *g += 1);
        // Point the matching block at this grid. Bump its id only the first
        // time (to force the keyed view list to rebuild from the empty
        // fallback into the inline terminal); later feeds just repaint.
        self.blocks.update(|blocks| {
            for block in blocks.iter_mut() {
                if let ChatBlock::ToolCall {
                    id,
                    tool_id: tid,
                    terminal_id: term,
                    ..
                } = block
                {
                    if tid == tool_id {
                        if term.is_none() {
                            *term = Some(tool_id.to_string());
                            *id = self.new_id();
                        }
                        return;
                    }
                }
            }
        });
        self.bump_scroll();
    }

    /// Attach a parsed `diff` content block (edit/write tools) to a tool call
    /// so the view renders a diff instead of a standard tool call. Bumps the
    /// block id to force the keyed view list to rebuild into the diff view.
    fn attach_diff_to_tool_call(&self, tool_id: &str, diff: &ToolDiff) {
        self.blocks.update(|blocks| {
            for block in blocks.iter_mut() {
                if let ChatBlock::ToolCall {
                    id,
                    tool_id: tid,
                    diff_path,
                    old_text,
                    new_text,
                    ..
                } = block
                {
                    if tid == tool_id {
                        *diff_path = Some(diff.path.clone());
                        *old_text = diff.old_text.clone();
                        *new_text = Some(diff.new_text.clone());
                        *id = self.new_id();
                        return;
                    }
                }
            }
        });
        self.bump_scroll();
    }

    /// Feed raw bytes from the proxy into a chat terminal's grid and bump
    /// its generation counter so the inline view repaints. If the grid
    /// doesn't exist yet (race: bytes arrive before the tool_call_update
    /// that carries the terminalId), create it now so no output is lost.
    pub fn handle_terminal_data(&self, terminal_id: &str, data: &[u8]) {
        tracing::debug!(
            terminal_id = %terminal_id,
            bytes = data.len(),
            "chat: terminal data"
        );
        // Ensure the handle exists — create on first byte if needed.
        let handle = {
            let mut terminals = self.terminals.borrow_mut();
            terminals
                .entry(terminal_id.to_string())
                .or_insert_with(|| ChatTermHandle {
                    raw: Arc::new(RwLock::new(ChatRawTerminal::new(24, 80))),
                    paint_gen: self.scope.create_rw_signal(0u64),
                })
                .clone()
        };
        if let Ok(mut grid) = handle.raw.write() {
            grid.feed(data);
        }
        handle.paint_gen.update(|g| *g += 1);
        self.bump_scroll();
    }

    /// Handle session disconnection.
    pub fn handle_disconnected(&self) {
        self.is_loading.set(false);
        self.session_id.set(None);
        self.push_block(self.system_block("Agent disconnected.".to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The exact shape crow-cli sends when an `execute` tool spawns a
    /// terminal: a `tool_call_update` with `status:"in_progress"` and a
    /// `content` array holding a single `Terminal` block (ACP schema
    /// `Terminal` = `{ type: "terminal", terminalId }`, camelCase on the
    /// wire). Extraction MUST yield the terminalId.
    #[test]
    fn extract_terminal_id_from_real_wire_json() {
        let update = json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "t1",
            "status": "in_progress",
            "content": [
                { "type": "terminal", "terminalId": "acp_term_1" }
            ]
        });
        assert_eq!(
            extract_terminal_id(&update),
            Some("acp_term_1".to_string())
        );
    }

    /// A terminal block alongside other content blocks is still found.
    #[test]
    fn extract_terminal_id_mixed_content() {
        let update = json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "t1",
            "content": [
                { "type": "content", "content": { "type": "text", "text": "hi" } },
                { "type": "terminal", "terminalId": "acp_term_7" }
            ]
        });
        assert_eq!(
            extract_terminal_id(&update),
            Some("acp_term_7".to_string())
        );
    }

    /// The final `completed` update carries no content → no terminal id.
    #[test]
    fn extract_terminal_id_completed_has_none() {
        let update = json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "t1",
            "status": "completed"
        });
        assert_eq!(extract_terminal_id(&update), None);
    }

    /// Content with only non-terminal blocks → None.
    #[test]
    fn extract_terminal_id_non_terminal_content() {
        let update = json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "t1",
            "content": [
                { "type": "content", "content": { "type": "text", "text": "x" } }
            ]
        });
        assert_eq!(extract_terminal_id(&update), None);
    }

    // ---- extract_content_text: the path crow-cli ACTUALLY uses ----

    /// The EXACT completed `tool_call_update` crow-cli v0.1.25 sends for an
    /// `execute` tool (captured from the proxy log). crow-cli runs its own
    /// PTY and returns the ANSI output as a wrapped text content block —
    /// there is NO terminal block. We must recover the text.
    #[test]
    fn extract_content_text_from_real_wire_json() {
        let update = json!({
            "content": [
                {
                    "content": {
                        "text": "\u{1b}[?2004l\rWed Jul 22 08:32:24 EDT 2026\r\n\u{1b}[?2004h\n[Command completed with exit code 0]",
                        "type": "text"
                    },
                    "type": "content"
                }
            ],
            "status": "completed",
            "toolCallId": "2e417c02/call_eb34",
            "sessionUpdate": "tool_call_update"
        });
        let text = extract_content_text(&update)
            .expect("real crow-cli execute output must yield text");
        assert!(text.contains("Wed Jul 22 08:32:24 EDT 2026"), "got: {text:?}");
        assert!(
            text.contains("[Command completed with exit code 0]"),
            "got: {text:?}"
        );
    }

    /// The in_progress update crow-cli sends has no content → no text.
    #[test]
    fn extract_content_text_in_progress_has_none() {
        let update = json!({
            "status": "in_progress",
            "toolCallId": "2e417c02/call_eb34",
            "sessionUpdate": "tool_call_update"
        });
        assert_eq!(extract_content_text(&update), None);
    }

    /// A bare `{type:"text", text}` block (some agents) is also accepted.
    #[test]
    fn extract_content_text_bare_text_block() {
        let update = json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "t1",
            "content": [ { "type": "text", "text": "hello" } ]
        });
        assert_eq!(extract_content_text(&update), Some("hello".to_string()));
    }

    /// Multiple text blocks concatenate in order.
    #[test]
    fn extract_content_text_concatenates() {
        let update = json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "t1",
            "content": [
                { "type": "content", "content": { "type": "text", "text": "foo\r\n" } },
                { "type": "content", "content": { "type": "text", "text": "bar" } }
            ]
        });
        assert_eq!(
            extract_content_text(&update),
            Some("foo\r\nbar".to_string())
        );
    }

    // ---- extract_diff: the edit/write tool path (ACP `Diff` block) ----

    /// The EXACT block crow-cli's `edit` tool sends (via
    /// `tool_diff_content(path, new_text, old_text)`): a `diff` content block
    /// with camelCase `newText`/`oldText`. `oldText` present ⇒ edit.
    #[test]
    fn extract_diff_edit_with_old_text() {
        let update = json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "edit_1",
            "status": "completed",
            "content": [
                {
                    "type": "diff",
                    "path": "/home/u/src/main.rs",
                    "newText": "fn main() {}\n",
                    "oldText": "fn main() { println!(\"hi\"); }\n"
                }
            ]
        });
        assert_eq!(
            extract_diff(&update),
            Some(ToolDiff {
                path: "/home/u/src/main.rs".to_string(),
                old_text: Some("fn main() { println!(\"hi\"); }\n".to_string()),
                new_text: "fn main() {}\n".to_string(),
            })
        );
    }

    /// crow-cli's `write` tool omits `old_text` ⇒ `oldText` absent ⇒ None
    /// (signals a brand-new file / full write, rendered as a new-file view).
    #[test]
    fn extract_diff_write_no_old_text() {
        let update = json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "write_1",
            "status": "completed",
            "content": [
                {
                    "type": "diff",
                    "path": "/home/u/new.rs",
                    "newText": "pub fn x() {}\n"
                }
            ]
        });
        assert_eq!(
            extract_diff(&update),
            Some(ToolDiff {
                path: "/home/u/new.rs".to_string(),
                old_text: None,
                new_text: "pub fn x() {}\n".to_string(),
            })
        );
    }

    /// `oldText: null` (pydantic may serialize None as null) is also None.
    #[test]
    fn extract_diff_write_null_old_text() {
        let update = json!({
            "content": [
                {
                    "type": "diff",
                    "path": "/a.rs",
                    "newText": "x",
                    "oldText": null
                }
            ]
        });
        assert_eq!(
            extract_diff(&update),
            Some(ToolDiff {
                path: "/a.rs".to_string(),
                old_text: None,
                new_text: "x".to_string(),
            })
        );
    }

    /// snake_case fallback (`new_text`/`old_text`), matching crow-ade's
    /// `newText ?? new_text` defensiveness.
    #[test]
    fn extract_diff_snake_case_fallback() {
        let update = json!({
            "content": [
                { "type": "diff", "path": "/b.rs", "new_text": "n", "old_text": "o" }
            ]
        });
        assert_eq!(
            extract_diff(&update),
            Some(ToolDiff {
                path: "/b.rs".to_string(),
                old_text: Some("o".to_string()),
                new_text: "n".to_string(),
            })
        );
    }

    /// A terminal/content-only update has no diff block ⇒ None.
    #[test]
    fn extract_diff_none_without_diff_block() {
        let update = json!({
            "content": [ { "type": "terminal", "terminalId": "t9" } ]
        });
        assert_eq!(extract_diff(&update), None);
        let no_content = json!({ "status": "completed" });
        assert_eq!(extract_diff(&no_content), None);
    }

    // ---- diff_lines: the inline edit/write diff renderer's data ----

    /// A write/new-file (old = "") renders every line as Added (green).
    #[test]
    fn diff_lines_write_all_added() {
        let lines = diff_lines("", "fn a() {}\nfn b() {}\n");
        assert_eq!(
            lines,
            vec![
                ChatDiffLine {
                    kind: ChatDiffLineKind::Added,
                    text: "fn a() {}".to_string()
                },
                ChatDiffLine {
                    kind: ChatDiffLineKind::Added,
                    text: "fn b() {}".to_string()
                },
            ]
        );
    }

    /// An edit: unchanged context, one removed line, one added line.
    #[test]
    fn diff_lines_edit_removed_and_added() {
        let lines = diff_lines("a\nb\nc\n", "a\nX\nc\n");
        // 'a' context, 'b' removed, 'X' added, 'c' context.
        assert!(lines.contains(&ChatDiffLine {
            kind: ChatDiffLineKind::Removed,
            text: "b".to_string()
        }));
        assert!(lines.contains(&ChatDiffLine {
            kind: ChatDiffLineKind::Added,
            text: "X".to_string()
        }));
        assert!(lines.contains(&ChatDiffLine {
            kind: ChatDiffLineKind::Context,
            text: "a".to_string()
        }));
        assert!(lines.contains(&ChatDiffLine {
            kind: ChatDiffLineKind::Context,
            text: "c".to_string()
        }));
        // No spurious removes/adds beyond the single changed line.
        let removed: Vec<_> = lines
            .iter()
            .filter(|l| l.kind == ChatDiffLineKind::Removed)
            .collect();
        let added: Vec<_> = lines
            .iter()
            .filter(|l| l.kind == ChatDiffLineKind::Added)
            .collect();
        assert_eq!(removed.len(), 1);
        assert_eq!(added.len(), 1);
    }

    /// Identical content → all context, no adds/removes.
    #[test]
    fn diff_lines_identical_all_context() {
        let lines = diff_lines("a\nb\n", "a\nb\n");
        assert!(!lines.is_empty());
        assert!(lines
            .iter()
            .all(|l| l.kind == ChatDiffLineKind::Context));
    }

    // ---- extract_file_path: clickable "open file" link source ----

    /// The diff block's `path` wins (edit/write tools).
    #[test]
    fn extract_file_path_prefers_diff_path() {
        let raw = Some(r#"{ "file_path": "/from/args.rs" }"#);
        assert_eq!(
            extract_file_path(Some("/from/diff.rs"), raw),
            Some("/from/diff.rs".to_string())
        );
    }

    /// Falls back to `file_path` in rawInput (crow-cli read/write/edit args).
    #[test]
    fn extract_file_path_from_raw_input_file_path() {
        let raw = Some(
            r#"{
  "file_path": "/home/u/src/main.rs",
  "offset": 1
}"#,
        );
        assert_eq!(
            extract_file_path(None, raw),
            Some("/home/u/src/main.rs".to_string())
        );
    }

    /// `path` is accepted as a fallback key (other agents).
    #[test]
    fn extract_file_path_from_raw_input_path_key() {
        let raw = Some(r#"{ "path": "/x/y.rs" }"#);
        assert_eq!(
            extract_file_path(None, raw),
            Some("/x/y.rs".to_string())
        );
    }

    /// No path anywhere ⇒ None (e.g. the `execute` terminal tool).
    #[test]
    fn extract_file_path_none_when_absent() {
        assert_eq!(extract_file_path(None, Some(r#"{ "command": "ls" }"#)), None);
        assert_eq!(extract_file_path(None, None), None);
        assert_eq!(extract_file_path(Some(""), None), None);
        assert_eq!(extract_file_path(None, Some("not json")), None);
    }
}
