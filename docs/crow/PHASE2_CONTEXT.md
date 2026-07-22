# Context: Phase 2 — Chat Panel in lapce-app

## What exists now (Phase 1 complete)

The backend is wired:
- `lapce-proxy/src/acp/` — AcpSessionManager, spawns crow-cli, JSON-RPC over stdio
- `lapce-rpc` — ProxyRequest::AcpCreateSession/AcpPrompt/AcpCancel, CoreNotification::AcpSessionUpdate/AcpToolRequest/AcpDisconnected
- `lapce-proxy/src/dispatch.rs` — handles ACP requests, forwards events to app via CoreNotification

## What to build

A chat panel in `lapce-app` that:
1. Has a text input at the bottom
2. Shows agent responses rendered via TypstView (from floem-typst crate)
3. Sends prompts to the proxy via RPC
4. Receives response chunks and pushes them to TypstView

## Step 1: Add PanelKind::Chat

In `lapce-app/src/panel/kind.rs`:
```rust
pub enum PanelKind {
    // ... existing variants ...
    Chat,  // NEW
}
```
Add the svg_name (use a chat icon or reuse an existing one) and default_position (PanelPosition::RightTop or BottomLeft).

## Step 2: Create the chat panel module

Create `lapce-app/src/panel/chat_view.rs` (or `lapce-app/src/chat/`):

```rust
pub struct ChatPanelData {
    pub scope: Scope,
    pub messages: RwSignal<Vec<ChatMessage>>,
    pub input_text: RwSignal<String>,
    pub session_id: RwSignal<Option<String>>,
    pub is_streaming: RwSignal<bool>,
    pub common: Rc<CommonData>,
}

pub enum ChatMessage {
    User { text: String },
    Assistant { typst_view: TypstView },
    Error { message: String },
}
```

The view:
```
stack (vertical)
├── scroll (message list)
│   ├── UserMessage (label, right-aligned or styled)
│   ├── AssistantMessage (TypstView — streaming typst render)
│   └── ...
├── input area (horizontal stack)
│   ├── text_input (floem text input or basic editor)
│   └── send button
```

## Step 3: Wire up the data flow

When user presses Enter/Send:
1. Read input_text
2. Add ChatMessage::User to messages
3. If no session: send ProxyRequest::AcpCreateSession, wait for response
4. Send ProxyRequest::AcpPrompt { session_id, content }
5. Set is_streaming = true
6. Create a new TypstView for the assistant response

When CoreNotification::AcpSessionUpdate arrives:
1. Extract chunk content
2. Push to the current assistant TypstView: `typst_view.push(chunk)`
3. Request repaint

When CoreNotification::AcpDisconnected or session done:
1. Set is_streaming = false
2. Call typst_view.flush()

## Step 4: Handle CoreNotification in the app

In `lapce-app/src/window_tab.rs` or wherever CoreNotifications are handled, add match arms for:
- `AcpSessionUpdate` → forward to chat panel data
- `AcpToolRequest` → (Phase 3, for now just log it)
- `AcpDisconnected` → mark session as done

## Key references

- Terminal panel pattern: `lapce-app/src/panel/terminal_view.rs` + `lapce-app/src/terminal/panel.rs`
- How panels are created: look at how TerminalPanelData is instantiated in window_tab.rs
- How CoreNotifications are received: look at how terminal updates flow from proxy to app
- floem-typst crate: `~/src/crow-team/floem-typst/` (add as path dependency to lapce-app)

## Dependencies to add to lapce-app/Cargo.toml:
```toml
floem-typst = { path = "../../floem-typst" }
```

## What NOT to do:
- Don't build tool call fixtures yet (Phase 3)
- Don't build rich text input yet (Phase 3) — a basic text input is fine
- Don't build @-mentions, image paste, etc.
- Focus on: type text → send → see Typst-rendered response streaming in

## Testing:
1. `cargo check -p lapce-app` passes
2. `cargo build` (full lapce) passes
3. Ideally: run lapce, open the chat panel, type a message, see it work
   (This requires crow-cli to be installed and a config file — may need manual testing)

Write notes to ~/src/crow-team/lapce/PHASE2_NOTES.md when done.
