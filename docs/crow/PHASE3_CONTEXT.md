# Context: Phase 3 — Rich Chat (Tool Calls, Terminals, Diffs)

## What exists now (Phase 2 complete)

The chat panel works:
- `lapce-app/src/chat.rs` — ChatData model (messages, session, streaming)
- `lapce-app/src/panel/chat_view.rs` — Chat panel view (message list + input + send)
- Messages render as simple text labels
- Assistant responses stream via TypstView

## What to build

Rich message rendering that matches what crow-ade's acpChat does (reference: `~/src/crow-team/crow-ade/src/vs/workbench/contrib/acpChat/browser/components/`):

### 1. Tool Call Fixtures (from toolCallItem.ts + toolCallGroup.ts)

When the agent calls a tool, show a collapsible fixture:
```
┌─ 🔧 terminal: "ls -la ~/src" ──────────── [✓ 0.3s] ─┐
│  total 48                                              │
│  drwxr-xr-x  5 user user 4096 Jul 19 16:00 .          │
│  -rw-r--r--  1 user user  523 Jul 19 15:58 Cargo.toml │
└────────────────────────────────────────────────────────┘
```

Data model:
```rust
pub struct ToolCall {
    pub id: String,
    pub name: String,        // "terminal", "read", "write", "edit", "web_search"
    pub title: String,       // human-readable summary
    pub status: ToolStatus,  // Running, Completed, Failed
    pub duration_ms: Option<u64>,
    pub output: ToolOutput,
    pub collapsed: RwSignal<bool>,
}

pub enum ToolOutput {
    Terminal { content: String },
    FileRead { path: String, content: String },
    FileWrite { path: String, diff: String },
    Search { query: String, results: String },
    Generic { content: String },
}
```

View: collapsible stack with header (icon + name + title + status) and body (output content).

### 2. Thinking Block (from thinkingBlock.ts)

Collapsible, dimmed block showing agent reasoning:
```
┌─ 💭 Thinking... ──────────────────────────── [▼] ─┐
│  (dimmed text of agent's reasoning)                │
└────────────────────────────────────────────────────┘
```

### 3. Permission Request (from permissionRequest.ts)

When the agent requests permission:
```
┌─ ⚠️ Agent wants to run: rm -rf /tmp/build ─────────┐
│  [Approve]  [Deny]  [Always Allow]                  │
└─────────────────────────────────────────────────────┘
```

Wire up: Approve/Deny sends ProxyNotification::AcpToolResponse back to the proxy.

### 4. Message Actions (from messageActions.ts)

Hover actions on messages:
- Copy (copy message text to clipboard)
- Retry (re-send the last user message)

### 5. Streaming Indicators

- Typing indicator while waiting for first chunk
- Cursor blink at the end of streaming text
- "Stop" button replaces "Send" while streaming

## How tool calls flow through the RPC

From Phase 1's data flow:
```
Agent calls tool → CoreNotification::AcpToolRequest { session_id, tool_name, args }
  → App shows permission UI (if needed) or auto-approves
  → User clicks Approve
  → ProxyNotification::AcpToolResponse { session_id, tool_call_id, result }
  → Agent continues
```

For now, auto-approve all tools (no permission UI yet). Just show the tool call fixture with output.

## How to parse tool calls from AcpSessionUpdate

The AcpSessionUpdate notification carries chunk_type + content. Chunk types from crow-cli:
- `"text"` — assistant text content (push to TypstView)
- `"thinking"` — reasoning content (thinking block)
- `"tool_call_start"` — tool call beginning (create fixture)
- `"tool_call_end"` — tool call finished (update fixture status)
- `"tool_output"` — tool output content (append to fixture)
- `"done"` — turn complete

Check the actual crow-cli ACP implementation for exact message format:
`~/src/crow-team/crow-cli/crow-cli/src/crow_cli/` — look for how it sends updates.

## Files to create/modify

- `lapce-app/src/chat.rs` — extend ChatMessage enum with tool calls, thinking
- `lapce-app/src/panel/chat_view.rs` — render tool call fixtures, thinking blocks
- `lapce-app/src/window_tab.rs` — handle AcpToolRequest (auto-approve for now)

## What NOT to do yet:
- Don't build embedded terminal rendering (just show terminal output as text)
- Don't build diff rendering (just show the diff text)
- Don't build @-mentions or image paste
- Don't build the permission UI (auto-approve)
- Focus on: tool call fixtures + thinking blocks + streaming indicators + stop button

## Testing:
- `cargo check -p lapce-app` passes
- The chat view renders tool call fixtures (can test with mock data)

Write notes to ~/src/crow-team/lapce/PHASE3_NOTES.md when done.
