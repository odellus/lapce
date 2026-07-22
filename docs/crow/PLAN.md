# PLAN.md — Crow IDE (Lapce + Typst + floem)

## Goal
Fork Lapce into a pure-Rust IDE with native ACP agent integration and Typst-powered rendering. Kill TypeScript, DOM, npx, LaTeX. One typesetting engine, one rendering pipeline, all Rust, all GPU-accelerated.

## Repos
- **Lapce fork:** `~/src/crow-team/lapce` (v0.4.6, Apache-2.0, branch: `main`)
- **Typst:** `~/src/crow-team/typst` (v0.15.1, Apache-2.0)
- **floem-typst:** `~/src/crow-team/floem-typst` (streaming Typst → floem View)
- **crow-ade:** `~/src/crow-team/crow-ade` (reference: ACP backend + chat UI)
- **crow-cli:** `~/src/crow-team/crow-cli` (ACP agent, bench system)

---

## Phase 0: Make floem-typst compile ✅

- [x] Fix `floem-typst` against actual typst 0.15.1 API
- [x] Get `cargo build` passing
- [x] Write a minimal example: floem window + TypstView + push("Hello $x^2$")
- [x] Verify: math renders, text renders, streaming works
- [x] Adapted to floem `31fa8f4` API (PaintCx, EventCx, Weight/Style, PointerWheel)

## Phase 1: ACP Backend in Lapce ✅

- [x] Port `crow-acp` into `lapce-proxy/src/acp/` (agent.rs + session.rs, ~600 LOC)
- [x] Add ACP RPC messages to `lapce-rpc` (ProxyNotification + CoreNotification)
- [x] Wire up: proxy spawns crow-cli, manages sessions (AcpSessionManager in dispatch.rs)
- [x] Event/tool forwarding threads (read_loop → CoreNotification)
- [x] Fixed: `crow-cli acp` (subcommand, not flag)

## Phase 2: Chat Panel Shell ✅

- [x] Add `PanelKind::Chat` to lapce-app (kind.rs, data.rs, view.rs)
- [x] Create chat panel: message list + text input + send button
- [x] Wire up: input → proxy → crow-cli → response → chat view
- [x] ChatData model in chat.rs, chat_view.rs panel view
- [x] AcpSessionCreated/Failed/Update/Disconnected handlers in window_tab.rs

## Phase 3: Rich Chat ✅

- [x] Typed ChatBlock enum: UserText, AssistantText, Thinking, ToolCall, PermissionRequest, System
- [x] ACP protocol parsing: agent_message_chunk, agent_thought_chunk, tool_call, tool_call_update
- [x] Tool call fixtures: status icons (○◐●✕), kind badge, expandable raw input/output
- [x] Thinking blocks: collapsible ▶/▼, italic dim
- [x] Permission UI: yellow warning box, Allow/Deny buttons, auto-approve reads
- [x] Stop button (red ■, sends AcpCancel)
- [x] Copy button on messages (⎘, floem Clipboard)
- [x] Auto-scroll (scroll_version signal, snaps to bottom on new content)
- [x] Streaming indicator (● ● ● Thinking...)
- [ ] Embedded terminal in tool calls (alacritty_terminal view) — deferred
- [ ] Diff view in tool calls (reuse DiffEditorData) — deferred
- [ ] @-mentions in input — deferred
- [ ] Image paste in input — deferred

## Phase 4: Typst Editor ✅

- [x] TypstPreview panel (PanelKind::TypstPreview, right side)
- [x] Live preview: create_effect watches doc buffer, debounced 150ms, recompiles via typst
- [x] PDF export: InternalCommand::ExportTypstPdf → floem_typst::render_to_pdf() → write .pdf
- [x] floem-typst adapted to lapce's floem rev (31fa8f4)
- [ ] Syntax highlighting for .typ files (tree-sitter grammar) — deferred
- [ ] WYSIWYG mode — deferred

## Phase 5: Skills + Notes Browser 🔄

- [x] Skills catalog in crow-cli (get_skills_catalog() in session.py)
- [x] `{{ skills_catalog }}` in system_prompt.jinja2 + orchestrator_prompt.jinja2
- [x] `skill-creation` meta-skill (~/.crow/skills/skill-creation/)
- [x] `learn` skill (~/.crow/skills/learn/) — SKILL.md + references + baseline.yaml
- [ ] Notes browser panel (PanelKind::NotesBrowser) — delegated to worker
- [ ] Session management (multiple sessions, history) — deferred
- [ ] Model/config picker — deferred
- [ ] Keyboard shortcuts — deferred

---

## Architecture Notes

### Streaming Typst → floem Pipeline
```
Agent streams Typst tokens
  → TypstStream::push(chunk)
  → Debounce (80ms)
  → TypstStream::tick():
      - Find block boundaries (\n\n outside fences/math)
      - Completed blocks: typst::compile() → Frame → freeze
      - Active tail: typst::compile() → Frame → hot region
  → TypstView::paint():
      - Frozen blocks: paint_frame() [cached, append-only]
      - Active tail: paint_frame() [re-rendered each tick]
  → floem/vger GPU render
```

### Key Design Decisions
- **Typst is the layout engine**, floem is the renderer
- **Agent generates Typst**, not markdown (system prompt instructs this)
- **No KaTeX, no mermaid, no marked.js** — Typst handles math, diagrams, tables natively
- **Append-only GPU rendering** — frozen blocks cost nothing, no reflow
- **floem_editor_core for input** — not Tiptap, not ProseMirror
- **alacritty_terminal for embedded terminals** — not xterm.js
- **crossbeam channels for IPC** — not Tauri, not websockets

### What We're Killing
TypeScript, npx, DOM, CSS, HTML, xterm.js, Tiptap, marked.js, KaTeX, Mermaid, Tauri IPC, VS Code extension host, LaTeX

### What We're Gaining
Pure Rust, GPU rendering (vger/wgpu), native typesetting (Typst), append-only streaming, 68K-line codebase, one typesetting engine, Apache-2.0, ACP as first-class citizen
