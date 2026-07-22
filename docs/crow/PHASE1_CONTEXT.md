# Context: Phase 1 — Port crow-acp into lapce-proxy

## What is crow-acp?

The ACP (Agent Client Protocol) client that manages agent sessions. It lives at:
`~/src/crow-team/crow-ade/crates/crow-acp/src/`

Files (3,707 lines total):
- `session.rs` (1,141 lines) — AcpSession: full session lifecycle, prompt queue, terminal management
- `prompt.rs` (458 lines) — Prompt lifecycle: serialize, run_prompt_turn, run_task_loop (nag loop)
- `manager.rs` (364 lines) — AcpSessionManager: spawn, bind, switch sessions
- `orchestration_state.rs` (378 lines) — Pure state machine: task_list, determine_next_prompt()
- `agent.rs` (340 lines) — Agent wrapper, config
- `tools/terminal.rs` (328 lines) — Terminal tool for agents
- `tools/orchestration.rs` (475 lines) — task_read, task_write, task_send
- `tools/filesystem.rs` (126 lines) — File read/write tools
- `tools/mod.rs` (66 lines) — Tool registry
- `tools/permissions.rs` (14 lines) — Permission stubs
- `lib.rs` (17 lines) — Module root

## What is lapce-proxy?

The backend process for Lapce. Lives at:
`~/src/crow-team/lapce/lapce-proxy/src/`

Files:
- `dispatch.rs` — Dispatcher: handles all proxy notifications/requests (terminals, buffers, plugins, git)
- `terminal.rs` — Terminal management (alacritty_terminal PTY)
- `plugin/mod.rs` — Plugin catalog
- `plugin/lsp.rs` — LSP server management
- `plugin/psp.rs` — Plugin server protocol
- `buffer.rs` — File buffer management
- `watcher.rs` — File system watcher
- `lib.rs`, `cli.rs` — Entry points

## Architecture: How they connect

Lapce uses a proxy architecture:
- `lapce-app` (UI) ↔ `lapce-proxy` (backend) via `lapce-rpc` (crossbeam channels)
- The Dispatcher handles `ProxyNotification` and `ProxyRequest` enums
- Adding ACP = adding new variants to these enums + a new module in the proxy

## What to do

### Step 1: Create the acp module in lapce-proxy

Create `~/src/crow-team/lapce/lapce-proxy/src/acp/` with:
- `mod.rs` — Module root, AcpManager struct
- `session.rs` — Port from crow-acp/session.rs (adapt to lapce's terminal/RPC)
- `prompt.rs` — Port from crow-acp/prompt.rs
- `orchestration.rs` — Port from crow-acp/orchestration_state.rs (this is pure logic, minimal changes)
- `tools.rs` — Port tool definitions (task_read, task_write, task_send, terminal, filesystem)

### Step 2: Add RPC messages

In `~/src/crow-team/lapce/lapce-rpc/src/proxy.rs`, add:
```rust
// New ProxyRequest variants:
AcpNewSession { config_path: PathBuf, session_id: String },
AcpPrompt { session_id: String, content: String },
AcpCancel { session_id: String },
AcpListSessions,

// New ProxyNotification variants (proxy → app):
AcpChunk { session_id: String, chunk_type: String, content: String },
AcpToolCallStart { session_id: String, tool_name: String, title: String },
AcpToolCallEnd { session_id: String, tool_name: String, status: String },
AcpSessionDone { session_id: String },
AcpError { session_id: String, message: String },
```

### Step 3: Wire into Dispatcher

In `dispatch.rs`, handle the new notifications:
- `AcpNewSession` → spawn AcpManager, create session
- `AcpPrompt` → forward to session
- `AcpCancel` → cancel session

### Key differences from crow-ade:

1. **Terminal:** crow-ade uses its own terminal crate. Lapce uses alacritty_terminal in `lapce-proxy/src/terminal.rs`. The ACP terminal tool should use lapce's existing terminal infrastructure.

2. **RPC:** crow-ade uses Tauri IPC. Lapce uses crossbeam channels via `lapce-rpc`. Session events flow back to the app via `CoreNotification`.

3. **Config:** crow-ade reads from VS Code settings. Lapce reads from TOML config. The ACP config (which crow-cli binary, which config YAML) should come from lapce's settings.

4. **No orchestration UI yet:** Just get the backend working. The app-side panel comes in Phase 2.

### What NOT to do:
- Don't modify lapce-app yet (that's Phase 2)
- Don't try to make the full orchestration loop work yet
- Focus on: create session → send prompt → get response chunks back via RPC
- The crow-cli binary is the actual agent — the proxy just spawns it and manages the ACP connection

### Reference: How crow-cli is invoked

From crow-ade, the agent is spawned as:
```
crow-cli acp --config-file <path>
```
This starts an ACP server over stdio. The proxy connects to it, creates sessions, sends prompts.

### Dependencies to add to lapce-proxy/Cargo.toml:
- The ACP protocol types (check what crow-acp uses — likely `acp` or `agent-client-protocol` crate)
- tokio or async-std for async process management (check what lapce-proxy already uses)
- serde/serde_json for message serialization

### Testing:
After wiring up, you should be able to:
1. Build lapce-proxy: `cargo build -p lapce-proxy`
2. The new AcpManager compiles and is reachable from the Dispatcher
3. (Manual test later): Send AcpNewSession + AcpPrompt via the RPC channel, see response chunks come back

Write a brief summary of what you did to ~/src/crow-team/lapce/PHASE1_NOTES.md when done.
