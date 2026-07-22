# Chat Input → Real Editor: Implementation Plan

> **Goal.** Replace the single-line `text_input` in the chat panel with a real,
> multi-line lapce editor: **8–12 lines** default, **drag-to-resize**, **Enter sends /
> Shift+Enter newline**, **syntax highlighting**, **insert-mode typing** (no vim modal
> weirdness), **clear on send**. Panel chat first; editor-tab chats reuse the same later.
>
> **Reference:** `docs/crow-ade-architecture-mapping.md` §9. crow-ade's `chatInput.ts`
> is a "rich text editor" (Zed-style); its core behavior (`chatInput.ts:165-167`) is
> `if (ke.key === 'Enter' && !ke.shiftKey) { send }`. We port that onto lapce's editor.

---

## THE KEY ARCHITECTURAL FACTS (verified — do not re-derive)

1. **`TextInputBuilder.build_editor` is SINGLE-LINE.** `crate::text_input::text_input_full`
   doc: *"Create a basic single line text input."* Used by find/replace. **Do NOT use it**
   for the multi-line chat input.
2. **Use `editor_view`** (`lapce-app/src/editor/view.rs:148`) — the real multi-line editor.
   Signature: `editor_view(e_data: EditorData, debug_breakline: Memo<Option<(usize, PathBuf)>>,
   is_active: impl Fn(bool) -> bool + 'static + Copy) -> EditorView`.
3. **Create the input editor** via `Editors::make_local(cx, common)` — exact precedent:
   `find_editor`/`replace_editor` at `main_split.rs:442-443` (`pub find_editor: EditorData`).
   `make_local` registers it in the editors registry (fine — find/replace do too; it's not a tab).
4. **Keys route through lapce's keypress system, NOT floem on_event.** `window_tab.rs::key_down`
   (~2497) matches on `Focus`. **`Focus::Panel(PanelKind::Chat)` has NO arm today** — it falls to
   `_ => None` then `keypress.key_down(event, self)`. We add an arm that routes to a custom
   `KeyPressFocus` wrapping the input editor.
5. **Enter interception (the crux):** the keypress resolves Enter → `EditCommand::InsertNewLine`
   (see `editor.rs:462`). Our `KeyPressFocus::run_command(command, count, mods)` intercepts
   `CommandKind::Edit(EditCommand::InsertNewLine)`: if `!mods.contains(Modifiers::SHIFT)` →
   `chat.send_prompt()` + return `CommandExecuted::Yes`; else delegate to the editor (newline).
   **VERIFY** the keymap binds Shift+Enter to InsertNewLine with the SHIFT mod; if Shift+Enter
   isn't bound, bind it or handle the raw key.
6. **Force insert mode:** `KeyPressFocus::get_mode()` returns `Mode::Insert` so typing always
   inserts (solves the "modal editing" concern — keys type text regardless of `core.modal`).
7. **Read buffer:** `doc.buffer.with(|b| b.to_string())`. **Clear:** `doc.reload(Rope::from(""), true)`
   (`doc.rs:481`). `doc` = `input_editor.doc()`.

---

## STAGES (compile + `cargo test -p lapce-app chat` after each)

### Stage A — ChatData owns the input editor
`lapce-app/src/chat.rs`:
- imports: `use crate::editor::EditorData; use crate::main_split::Editors; use lapce_xi_rope::Rope;`
  (and `KeyPressFocus`/`Mode`/`CommandKind`/`EditCommand`/`Modifiers`/`CommandExecuted`/`Condition`
  for Stage B).
- struct: replace `pub input: RwSignal<String>` with `pub input_editor: EditorData`.
- `new`/`new_with_id`: add param `editors: Editors`; set
  `input_editor: editors.make_local(cx, common.clone())`; drop the `input` signal init.
- `send_prompt`: `let text = self.input_editor.doc().buffer.with(|b| b.to_string()).trim().to_string();`
  … after sending, `self.input_editor.doc().reload(Rope::from(""), true);` (replaces `self.input.update(clear)`).

Call sites — pass editors:
- `window_tab.rs:404` `ChatData::new(cx, common.clone())` → add `main_split.editors` (main_split is
  built just above; `source_control` already uses `main_split.editors` there).
- `window_tab.rs::editor_chat` `ChatData::new_with_id(self.scope, self.common.clone(), id)` →
  add `self.main_split.editors`.

### Stage B — key routing + Enter/Shift+Enter (the crux)
- New `ChatInputFocus` (put in `chat.rs` or a new `chat_input.rs`):
  ```rust
  #[derive(Clone, Debug)]
  pub struct ChatInputFocus { pub editor: EditorData, pub chat: ChatData }
  impl KeyPressFocus for ChatInputFocus {
      fn get_mode(&self) -> Mode { Mode::Insert }
      fn check_condition(&self, c: Condition) -> bool { self.editor.check_condition(c) }
      fn run_command(&self, cmd: &LapceCommand, count: Option<usize>, mods: Modifiers) -> CommandExecuted {
          if let CommandKind::Edit(EditCommand::InsertNewLine) = &cmd.kind {
              if !mods.contains(Modifiers::SHIFT) { self.chat.send_prompt(); return CommandExecuted::Yes; }
          }
          self.editor.run_command(cmd, count, mods)
      }
      fn receive_char(&self, c: &str) { self.editor.receive_char(c); }
  }
  ```
  (Mirror the exact `KeyPressFocus` trait shape in `keypress.rs`; `EditorData` already impls it.)
- `window_tab.rs::key_down`: add arm
  ```rust
  Focus::Panel(PanelKind::Chat) => {
      let focus = ChatInputFocus { editor: self.chat.input_editor.clone(), chat: self.chat.clone() };
      Some(keypress.key_down(event, &focus))
  }
  ```

### Stage C — render the editor in the chat input area
`lapce-app/src/panel/chat_view.rs` (the `// Input area` block, ~line 158):
- Replace `text_input(input_text)…` + the `Send` button row with a container holding
  `editor_view(chat.input_editor.clone(), debug_breakline_memo, is_active)`.
  - `debug_breakline_memo = create_memo(move |_| None::<(usize, std::path::PathBuf)>)`.
  - `is_active = move |_| focus.get() == Focus::Panel(PanelKind::Chat)` (focus is in scope).
- Keep a small "Send"/"Stop" affordance if desired (Enter is the primary send).
- Border/padding to look like an input box.

### Stage D — height (8–12 lines) + drag-to-resize
- Add `input_height: RwSignal<f64>` to ChatData (default ≈ 10 lines × line-height; ~190–210px).
- Style the editor container `.height(move || input_height.get())`.
- Add a thin drag handle above the input: on `PointerDown` capture start y + start height;
  on `PointerMove` (while dragging) set `input_height = start_h + (start_y - cur_y)` (drag up = taller);
  clamp to a min (~3 lines) and max (~30 lines). Use `CursorStyle::RowResize`. Reference for drag
  mechanics: panel resize uses `window_tab_data.common.dragging` (`panel/view.rs:219`), but a local
  pointer-capture drag is fine here.

### Stage E — focus + polish
- Clicking the editor sets `Focus::Panel(PanelKind::Chat)` and requests floem focus so the caret shows.
- Set the input doc language to **markdown** so fenced code blocks highlight (find the doc syntax-setter;
  `Doc` syntax setup in `doc.rs`). Optional but nice.
- Ensure the editor doesn't show tab chrome / line numbers (editor_view is bare content — good).

---

## TEST / VERIFY
- `cargo check -p lapce-app` after each stage (only acceptable warning: pre-existing `unused variable: res`
  in `app/logging.rs:140`).
- `cargo test -p lapce-app chat` (13 existing tests must stay green).
- Build: `cargo build --bin lapce` (local runs the proxy in-process, so this suffices).
- **Cannot see the UI** — hand to Thomas to click-test: type multi-line, Enter sends, Shift+Enter newline,
  drag-resize, syntax highlight. If misbehaves, read `~/.local/share/lapce-debug/logs/lapce.*.log`.

## GOTCHAS
- floem scope rule: signals that outlive an effect run are allocated on the long-lived `Scope`, never bare
  in a callback (or the next paint panics on a dead `.get()`).
- `gen` is a reserved keyword — don't name anything `gen`.
- No permission system exists; never add permission_request.
- Editor-tab chats: their input sits under `Focus::Workbench`; `main_split.key_down` returns None for
  `EditorTabChild::Chat`. Out of scope for now — panel chat first. Note it for follow-up.
