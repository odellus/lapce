# crow-ade → lapce Architecture Mapping (the Rosetta Stone)

> **Purpose.** We are porting the Crow agent experience from **crow-ade** (a Tauri
> app whose frontend is the VS Code workbench in TypeScript) to **lapce** (a native
> Rust editor whose UI is `floem`). This document is the single source of truth for
> *how the two architectures map*, so that every feature port reduces to mechanical
> API-matching plus one well-understood frontend re-expression. Get this picture
> right and each feature is a "single shot"; the rest is minutiae.
>
> **Status:** living document. Update it as we port features.

---

## 1. The two apps at a glance

| | **crow-ade** (source) | **lapce** (target) |
|---|---|---|
| Frontend tech | VS Code **workbench** (TypeScript, DOM) | **floem** (native Rust, GPU-rendered) |
| Frontend shell | **Tauri** webview (`src-tauri/`, `frontendDist: ../dist`) | none — floem renders natively, **no webview** |
| Backend | **`crates/crow-acp`** (Rust, the Tauri core) | **`lapce-proxy`** (Rust; runs in-process for local workspaces) |
| Frontend↔backend seam | **Tauri IPC** (`invoke_handler` commands + `emit` events) | **`CoreNotification` / `ProxyNotification`** RPC (`lapce-rpc`) |
| Editor at the core | Monaco (← CodeMirror ← ProseMirror lineage) | lapce's floem editor (rope-backed `Doc`/`Buffer`) |

The backend halves are *basically the same thing* (a Rust ACP client that spawns the
agent and speaks ACP over stdio). The frontend halves solve the same job with
different-generation reactive systems. **That is the whole port.**

---

## 2. The three-layer migration model

```
┌─ FRONTEND ─  acpChat (TS, VS Code workbench)  →  floem views (native Rust)     ← the CRAFT
├─ SEAM ─────  Tauri IPC (commands + events)     →  Core/Proxy RPC notifications  ← minutiae
└─ BACKEND ──  crow-acp (Tauri Rust core)        →  lapce-proxy/acp               ← minutiae (mostly done)
```

- **Backend (minutiae):** copy a `crow-acp` method into `lapce-proxy/acp`. The module
  already shadows `crow-acp`'s shape. Mostly done.
- **Seam (minutiae):** for each Tauri command/event, define the matching
  `ProxyNotification` (client→proxy) or `CoreNotification` (proxy→client) in
  `lapce-rpc`, and wire it in `dispatch.rs` (proxy side) + `window_tab.rs` (client side).
- **Frontend (the craft):** we do **not** translate TS lines. We re-express *behavior*
  across two reactive systems (see the Rosetta Stone, §6). Read the acpChat component's
  state + event wiring; rebuild it as floem signals + views calling the same backend
  method over an RPC notif instead of a Tauri command.

---

## 3. Backend mapping — `crow-acp` ↔ `lapce-proxy/acp`

| crow-ade `crates/crow-acp/src/` | lapce `lapce-proxy/src/` | notes |
|---|---|---|
| `agent.rs` | `acp/agent.rs` | spawn + stdio transport to the agent |
| `session.rs` | `acp/session.rs` | session lifecycle, `initialize`, prompt, `set_config_option` |
| `manager.rs` | `dispatch.rs` (`self.acp_manager`) | multi-session registry |
| `prompt.rs` | prompt flow in `acp/session.rs` + `dispatch.rs` | streaming updates |
| `tools/terminal.rs` (+ `crow-terminal`) | `acp/terminal.rs` + `acp/pty.rs` | client-side terminal tool (real PTY) |
| `tools/filesystem.rs` | `dispatch.rs` `handle_acp_tool` `readTextFile`/`writeTextFile` arms | client-side fs tools |
| `orchestration_state.rs`, `tools/orchestration.rs` | (not yet ported) | future |
| `tools/permissions.rs` | **deleted — no permission system** | do not re-add |

### Client-side tools read/write the DOCUMENT MODEL, not raw disk
This is the point of them being *client* tools:
- `readTextFile` returns the **live rope buffer** (`self.buffers.get(path).get_document()`),
  falling back to disk only if the file isn't open. Unsaved edits are visible to the agent.
- `writeTextFile` writes **through** the doc model (updates the rope + pushes
  `open_file_changed`) *and* to disk, so the editor shows the change live with no desync.
- **Verify, don't assume:** confirm `self.buffers` is populated for the files the agent
  reads (read the proxy log after a live read of an unsaved buffer).

---

## 4. The seam — Tauri IPC ↔ `Core/Proxy` RPC

crow-ade: `src-tauri/src/lib.rs` registers `invoke_handler(generate_handler![...])`
(commands the frontend calls) and `emit`s events back. lapce: the same traffic is two
enums in `lapce-rpc`:

- `ProxyNotification` (frontend → proxy): e.g. `AcpCreateSession`, `AcpPrompt`, `AcpCancel`.
- `CoreNotification` (proxy → frontend): e.g. `AcpSessionCreated`, `AcpSessionUpdate`,
  `AcpTerminalData`, `AcpTerminalExit`.

**Multi-chat routing (done):** `AcpCreateSession` carries a `chat_id` token echoed in
`AcpSessionCreated`/`Failed`; every later notif carries `session_id`. `WindowTabData`
keeps `session_id→chat` and `token→chat` maps; unknown keys fall back to the panel chat.

---

## 5. Frontend mapping — `acpChat` ↔ `lapce-app` chat

| crow-ade `src/vs/workbench/contrib/acpChat/browser/` | lapce `lapce-app/src/` | role |
|---|---|---|
| `acpStore.ts` / `acpChatService.ts` | `chat.rs` (`ChatData`) | the chat data model |
| `acpChatView.ts` / `messageList.ts` | `panel/chat_view.rs` | message list + layout |
| `acpChatSessionManager.ts` | `window_tab.rs` (routing) | session→chat ownership |
| `acpChatEditor.ts` / `acpChatEditorInput.ts` | `editor_tab.rs` (`EditorTabChild::Chat`) | chat as an editor tab (+ tab naming) |
| `components/tools/toolCallItem.ts` | `panel/chat_view.rs` `render_tool_call` | a tool-call block |
| `components/tools/inlineTerminal.ts` | `chat_terminal.rs` | inline terminal grid |
| `components/toolbar/chatHeader.ts` | chat header in `chat_view.rs` | toolbar: **model selector** lands here |
| `components/input/chatInput.ts` | input area in `chat_view.rs` | **the input — see §9** |
| `inline/inlineEditController.ts` | (not yet ported) | **diff view for edit/write** |
| `media/acpChatView.css` | `.style(\|s\| …)` decorators | styling |

---

## 6. The UI-framework Rosetta Stone

**floem** = fine-grained **signals** (closer to SolidJS than React — no virtual DOM;
reads auto-track). **VS Code workbench** = a hand-rolled reactive component system built
around Monaco: `Emitter`/`Event` reactivity, service DI, `IDisposable` lifecycles,
`ViewPane`/`Part` containers, imperative DOM + CSS. Different generation, same job.

| VS Code workbench (acpChat) | floem (our chat) |
|---|---|
| `Emitter<T>.fire()` / `Event<T>` | `RwSignal.set()` / `.get()` (auto-tracking) |
| derived/observable state | `Memo` / `create_effect` |
| `IDisposable` / `DisposableStore` | `Scope` + `on_cleanup` |
| service DI (`IChatService`, `@IService`) | `Rc<CommonData>` / `WindowTabData` passed down (or provide/inject) |
| `ViewPane.render(body)` building DOM | a view fn returning `container(stack(…))` |
| CSS class / `media/acpChatView.css` | `.style(\|s\| …)` decorators |
| `WorkbenchContribution` startup hook | signal/effect init in `WindowTabData::new` |
| Monaco editor widget | lapce's floem editor (`EditorData` + `editor_view`) |

**Scope rule (hard-won):** any signal that outlives one effect run is allocated on the
long-lived `Scope` (`self.scope.create_rw_signal`), never bare inside a callback — or
floem disposes it on the next effect re-run and the next paint panics on a dead `.get()`.

---

## 7. Worked template — the terminal port (proof the model works)

The inline terminal is the canonical example; every future port follows this shape.

| layer | crow-ade | lapce | joined by |
|---|---|---|---|
| backend | `tools/terminal.rs` + `crow-terminal` | `acp/terminal.rs` (`AcpTerminal`) + `acp/pty.rs` (real PTY) | ACP `terminal/*` client-tool requests |
| seam | Tauri terminal events | `CoreNotification::AcpTerminalData` / `AcpTerminalExit` | RPC |
| frontend | `components/tools/inlineTerminal.ts` | `chat_terminal.rs` (`ChatRawTerminal` grid) + `render_tool_call` | `terminal_id` on the tool-call block |

Wire shapes (copy exactly): `terminal/create {command,…}` → `{terminalId}`;
`terminal/output` → `{output,truncated,exitStatus?}`; `terminal/waitForExit` →
`{exitCode,signal}` (must block until exit); `terminal/kill`/`release` → `{}`.

---

## 8. Per-feature plan template

Every feature plan is three bullets:

1. **Backend half** — copy `crow-acp/<file>::<method>` into `lapce-proxy/acp/…`.
2. **Seam** — add/extend the `Proxy`/`CoreNotification` that joins them.
3. **Frontend half** — re-express `acpChat/<component>.ts` per the Rosetta Stone (§6),
   calling the backend over the notif instead of a Tauri command.

### Feature backlog (two-half breakdowns)

| feature | backend half | seam | frontend half |
|---|---|---|---|
| **Model selector** | `session.rs::set_config_option(config_id="model", value)` (crow-acp `session.rs:801`) → `lapce-proxy/acp/session.rs` | RPC for "get config options" + "set option" | `chatHeader.ts` model menu → floem dropdown in chat header |
| **Diff view (edit/write)** | capture before/after in the write-through-doc path (crow-ade `tools/filesystem.rs`) | RPC notif carrying the diff (old/new text) | `inline/inlineEditController.ts` → floem diff block in the tool call |
| **Session id in tab name** | (none / session id already known) | (none) | `acpChatEditorInput.ts` naming → `EditorTabChild::Chat` `view_info` name |
| **Chat input as real editor** | (none — pure frontend) | (none) | `chatInput.ts` "rich text editor" → embed lapce `EditorData` (see §9) |

---

## 9. Chat input as a real lapce editor (design)

**Insight (Thomas):** the input shouldn't be a plain `text_input`. Monaco ← CodeMirror ←
ProseMirror — an editor *is* rich text. crow-ade's `chatInput.ts` is a "rich text editor"
(Zed-style). So lapce's input should be **a real lapce editor**, giving multi-line,
syntax highlighting, proper cursor/selection, undo/redo, and keybindings for free.

**How (all frontend, no backend/seam):**
- Create an in-memory editor: `Editors::new_local(cx, common)` →
  `EditorData::new_local(cx, editors, common)` (backs onto `Doc::new_local`,
  i.e. `DocContent::Local` — no file).
- Render it with `editor_view(e_data, debug_breakline, is_active)`
  (`lapce-app/src/editor/view.rs:148`) inside the chat input area.
- Intercept **Enter** (no Shift) → read the buffer text, send as the prompt, clear the
  buffer; **Shift+Enter** inserts a newline.
- Style it input-like: no line numbers / minimal chrome, compact padding, auto-grow
  height if feasible.

**Tradeoffs to decide:** modal editing on/off in the input; whether to keep a lightweight
fallback; auto-height vs fixed scroll. Reference: `chatInput.ts` for behavior, lapce's own
scratch/local editors for the embed mechanics.

---

## 10. Ground truth & methodology (non-negotiable)

- **Read the reference before changing behavior.** Backend ref: `crates/crow-acp/src/*`.
  Frontend ref: `src/vs/workbench/contrib/acpChat/browser/*`. Secondary ref: `~/src/crow-team/zed`.
- **Never guess the wire format** — read the ACP SDK schema (Python
  `acp/schema.py`; Rust `agent-client-protocol-schema`). Field aliases/types are exact.
- **The proxy log is ground truth:** `~/.local/share/lapce-debug/logs/lapce.YYYY-MM-DD.log`
  has every raw ACP message (`ACP <<`/`ACP >>`) + our `tracing` lines + crow-cli stderr.
- **Test + run + read results.** Write a failing test that reproduces the bug; `cargo check`
  / `cargo test` after each change; build the `lapce` binary (local runs the proxy
  in-process, so `--bin lapce` suffices; add `--bin lapce-proxy` only for remote workspaces).
- **No permission system exists** — never re-add or mention permission_request.
