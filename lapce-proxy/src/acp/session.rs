//! ACP session — speaks JSON-RPC over the agent's stdio.
//!
//! Ported from crow-acp's session.rs, adapted to sync threads + crossbeam.
//! Handles the ACP protocol lifecycle: initialize → session/new → prompt.
//! Client tool requests (fs, terminal) are forwarded to the Dispatcher.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, bounded};
use serde_json::{Value, json};

use super::agent::{AgentConfig, AgentManager};
use super::orchestration::{
    OrchestrationState, PromptTurnState, QueueItem,
};
use crate::acp_log;
use lapce_rpc::proxy::{ProxyNotification, ProxyRpcHandler};

/// Events broadcast to the UI when something happens in a session.
#[derive(Clone, Debug)]
pub enum SessionEvent {
    /// A session/update notification from the agent.
    Update {
        session_id: String,
        update: Value,
    },
    /// The agent process exited or the connection was lost.
    Disconnected {
        session_id: String,
    },
}

/// An ACP session connected to an agent subprocess.
pub struct AcpSession {
    /// Unique connection identifier.
    pub connection_id: String,
    /// The agent subprocess ID.
    pub agent_id: String,
    /// The ACP session ID (set after session/new or session/load).
    session_id: Mutex<Option<String>>,
    /// The agent's session config options (e.g. the model selector), last seen
    /// in a `session/new`/`session/load`/`session/set_config_option` response or
    /// a `config_option_update`. Exposed so the UI can render a model picker.
    config_options: Mutex<Option<Value>>,
    /// Agent config (for re-spawning on session switch).
    pub agent_config: AgentConfig,
    /// Send JSON-RPC messages to the agent.
    agent_manager: Arc<AgentManager>,
    /// Pending JSON-RPC request callbacks.
    pending: Arc<Mutex<HashMap<u64, Sender<Result<Value, Value>>>>>,
    /// Next JSON-RPC request ID.
    next_id: AtomicU64,
    /// The JSON-RPC id of the in-flight `session/prompt` request, if any.
    /// Its response signals end-of-turn (it is *not* registered in `pending`
    /// because `prompt_async` is fire-and-forget).
    active_prompt_id: Mutex<Option<u64>>,
    /// Outgoing session events (to UI).
    event_tx: Sender<SessionEvent>,
    /// The project root (cwd) advertised at session/new. Stored so we send the
    /// real workspace path, not the proxy process's own current directory.
    cwd: String,
    /// Injects synthetic `ProxyNotification`s into the Dispatcher's loop. ACP
    /// client-tool requests (fs/terminal) are routed through here so they run
    /// on the dispatch thread that owns the open document model (`buffers`) —
    /// reads return the live unsaved buffer, writes update the editor.
    proxy_rpc: ProxyRpcHandler,

    // ─── Orchestration / queue fields ──────────────────────────────────────

    /// Pure orchestration state machine (task lists, caller session id).
    pub orchestration: Mutex<OrchestrationState>,
    /// Whether a prompt turn is currently in flight (serializes prompts).
    pub prompt_busy: Mutex<bool>,
    /// Prompts that arrived while busy — drained after each turn.
    pub queue: Mutex<Vec<QueueItem>>,
    /// Prompt turn lifecycle state (broadcast to UI).
    pub prompt_turn_state: Mutex<PromptTurnState>,
    /// Guard flag: true while the worker task loop is running.
    pub task_loop_running: AtomicBool,
    /// Guard flag: true while the orchestrator task loop is running.
    pub orchestrator_task_loop_running: AtomicBool,
    /// Channel for synchronous prompt (task loop blocks on this).
    prompt_sync_tx: Mutex<Option<Sender<Value>>>,
}

impl AcpSession {
    /// Spawn an agent, start the stdout reader loop, and return a session.
    pub fn spawn(
        agent_manager: &Arc<AgentManager>,
        config: AgentConfig,
        cwd: &str,
        event_tx: Sender<SessionEvent>,
        proxy_rpc: ProxyRpcHandler,
    ) -> Result<Arc<Self>> {
        let agent_id = agent_manager.spawn(&config, cwd)?;
        let stdout_rx = agent_manager
            .take_stdout_rx(&agent_id)
            .context("No stdout receiver")?;

        let connection_id = format!("conn_{}", &agent_id);

        let session = Arc::new(Self {
            connection_id: connection_id.clone(),
            agent_id: agent_id.clone(),
            session_id: Mutex::new(None),
            config_options: Mutex::new(None),
            agent_config: config,
            agent_manager: agent_manager.clone(),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            active_prompt_id: Mutex::new(None),
            event_tx,
            cwd: cwd.to_string(),
            proxy_rpc,
            orchestration: Mutex::new(OrchestrationState::default()),
            prompt_busy: Mutex::new(false),
            queue: Mutex::new(Vec::new()),
            prompt_turn_state: Mutex::new(PromptTurnState::default()),
            task_loop_running: AtomicBool::new(false),
            orchestrator_task_loop_running: AtomicBool::new(false),
            prompt_sync_tx: Mutex::new(None),
        });

        // Start the stdout reader loop in a background thread.
        let session_clone = session.clone();
        thread::Builder::new()
            .name(format!("acp-reader-{}", connection_id))
            .spawn(move || {
                session_clone.read_loop(&stdout_rx);
            })?;

        Ok(session)
    }

    /// The ACP session ID (available after new_session/load_session).
    pub fn session_id(&self) -> String {
        self.session_id
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| self.connection_id.clone())
    }

    /// The working directory this session was created with (advertised at
    /// `session/new`/`session/load`). Used to list sessions for the same cwd.
    pub fn cwd(&self) -> String {
        self.cwd.clone()
    }

    /// The last-seen session config options (e.g. the model selector), as a
    /// JSON array (empty array if the agent never advertised any).
    pub fn config_options(&self) -> Value {
        self.config_options
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| json!([]))
    }

    /// Send `session/set_config_option` (e.g. switch the model). Returns the
    /// refreshed `configOptions` array from the response (empty array if the
    /// agent omitted it) and caches it. Mirrors crow-acp's `set_config_option`.
    pub fn set_config_option(
        &self,
        config_id: &str,
        value: &str,
    ) -> Result<Value> {
        let params = json!({
            "sessionId": self.session_id(),
            "configId": config_id,
            "value": value,
        });
        let resp = self.request("session/set_config_option", params)?;
        let opts = resp
            .get("configOptions")
            .cloned()
            .unwrap_or_else(|| json!([]));
        *self.config_options.lock().unwrap() = Some(opts.clone());
        Ok(opts)
    }

    /// Send the ACP `initialize` request.
    pub fn initialize(&self) -> Result<Value> {
        // Two wire details that MUST match the ACP SDK or the agent silently
        // drops our capabilities (verified against the `acp` Python SDK's
        // `InitializeRequest` model and crow-ade's `ProtocolVersion::LATEST`):
        //  * `protocolVersion` is an INTEGER (the SDK `PROTOCOL_VERSION = 1`),
        //    not a string like "0.4" — a string fails the `int` field parse.
        //  * the capabilities field is `clientCapabilities` (the SDK alias),
        //    NOT `capabilities`. Sending `capabilities` is an unknown key that
        //    pydantic ignores, so the agent defaults to `terminal=False` and
        //    runs its own MCP terminal instead of our client-side one.
        let params = json!({
            "protocolVersion": 1,
            "clientInfo": {
                "name": "lapce",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "clientCapabilities": {
                "fs": { "readTextFile": true, "writeTextFile": true },
                "terminal": true,
            }
        });
        self.request("initialize", params)
    }

    /// Send `session/new` and store the session ID.
    pub fn new_session(&self, mcp_servers: Vec<Value>) -> Result<Value> {
        let params = json!({
            "cwd": self.cwd,
            "mcpServers": mcp_servers,
        });
        let resp = self.request("session/new", params)?;
        if let Some(sid) = resp.get("sessionId").and_then(|v| v.as_str()) {
            *self.session_id.lock().unwrap() = Some(sid.to_string());
        }
        *self.config_options.lock().unwrap() =
            resp.get("configOptions").cloned();
        Ok(resp)
    }

    /// Send `session/load` for an existing session.
    pub fn load_session(&self, target_session_id: &str, cwd: &str) -> Result<Value> {
        let params = json!({
            "sessionId": target_session_id,
            "cwd": cwd,
        });
        let resp = self.request("session/load", params)?;
        *self.session_id.lock().unwrap() = Some(target_session_id.to_string());
        *self.config_options.lock().unwrap() =
            resp.get("configOptions").cloned();
        Ok(resp)
    }

    /// Send a prompt to the agent.
    pub fn prompt(&self, content: &str) -> Result<Value> {
        let session_id = self.session_id();
        let params = json!({
            "sessionId": session_id,
            "prompt": [{
                "type": "text",
                "text": content,
            }],
        });
        self.request("session/prompt", params)
    }

    /// Send a prompt asynchronously (fire-and-forget, results come via events).
    pub fn prompt_async(&self, content: &str) -> Result<()> {
        let session_id = self.session_id();
        let params = json!({
            "sessionId": session_id,
            "prompt": [{
                "type": "text",
                "text": content,
            }],
        });
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/prompt",
            "params": params,
        });
        *self.active_prompt_id.lock().unwrap() = Some(id);
        self.send_raw(&msg)?;
        // Don't wait for response — it comes as session/update notifications.
        Ok(())
    }

    /// Cancel the current prompt.
    pub fn cancel(&self) -> Result<()> {
        // ACP `session/cancel` is a NOTIFICATION (no `id`). The agent SDK
        // registers it in the notification table; sending it as a request
        // makes the router look it up as a request, miss, and never invoke
        // the cancel handler — so the prompt keeps running.
        let session_id = self.session_id();
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "session/cancel",
            "params": { "sessionId": session_id },
        });
        self.send_raw(&msg)
    }

    // ─── Queue + task loop (ported from crow-ade prompt.rs) ────────────────

    /// Send a prompt with content blocks. If busy, queue and return Ok.
    /// Otherwise run the turn + drain queue + task loops.
    ///
    /// This is the sync equivalent of crow-ade's `prompt()`. It runs on the
    /// caller's thread (the dispatch thread for user prompts, or a spawned
    /// thread for task_send callbacks).
    pub fn prompt_with_queue(&self, blocks: Vec<Value>) -> Result<()> {
        // Try to acquire the busy lock.
        {
            let mut busy = self.prompt_busy.lock().unwrap();
            if *busy {
                self.queue.lock().unwrap().push(QueueItem::Prompt(blocks));
                acp_log!(
                    "INFO",
                    "prompt_with_queue: session {} busy, queued (len={})",
                    self.session_id(),
                    self.queue.lock().unwrap().len()
                );
                self.broadcast_queue_state();
                return Ok(());
            }
            *busy = true;
        }

        // Run the turn + drain.
        let result = self.run_prompt_turn(blocks);
        *self.prompt_busy.lock().unwrap() = false;
        result
    }

    /// Inner: run one prompt turn, then task loops, then drain queue.
    fn run_prompt_turn(&self, blocks: Vec<Value>) -> Result<()> {
        let stop_reason = self.prompt_sync_blocks(&blocks)?;

        if stop_reason == "cancelled" {
            acp_log!(
                "INFO",
                "prompt: turn cancelled for session {}, pausing",
                self.session_id()
            );
            return Ok(());
        }

        // Task loop
        if self.should_run_task_loop() {
            self.run_task_loop()?;
        }

        // Orchestrator task loop
        if self.should_run_orchestrator_task_loop() {
            self.run_orchestrator_task_loop()?;
        }

        // Drain queued prompts
        loop {
            let next = {
                let mut q = self.queue.lock().unwrap();
                if q.is_empty() {
                    break;
                }
                q.remove(0)
            };
            self.broadcast_queue_state();
            match next {
                QueueItem::Prompt(blocks) => {
                    let sr = self.prompt_sync_blocks(&blocks)?;
                    if sr == "cancelled" {
                        break;
                    }
                    if self.should_run_task_loop() {
                        self.run_task_loop()?;
                    }
                    if self.should_run_orchestrator_task_loop() {
                        self.run_orchestrator_task_loop()?;
                    }
                }
                QueueItem::Task(task) => {
                    let blocks = vec![json!({
                        "type": "text",
                        "text": format!("Task: {}", task.title),
                    })];
                    let sr = self.prompt_sync_blocks(&blocks)?;
                    if sr == "cancelled" {
                        break;
                    }
                    if self.should_run_task_loop() {
                        self.run_task_loop()?;
                    }
                    if self.should_run_orchestrator_task_loop() {
                        self.run_orchestrator_task_loop()?;
                    }
                }
            }
        }

        Ok(())
    }

    /// Send a prompt with content blocks and block until the response.
    /// Returns the stopReason string.
    fn prompt_sync_blocks(&self, blocks: &[Value]) -> Result<String> {
        let session_id = self.session_id();
        let params = json!({
            "sessionId": session_id,
            "prompt": blocks,
        });
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/prompt",
            "params": params,
        });

        // Set up the sync channel before sending.
        let (tx, rx) = bounded(1);
        *self.prompt_sync_tx.lock().unwrap() = Some(tx);
        *self.active_prompt_id.lock().unwrap() = Some(id);

        // Broadcast running state.
        {
            let mut state = self.prompt_turn_state.lock().unwrap();
            *state = PromptTurnState::Running;
        }
        self.broadcast_prompt_state();

        self.send_raw(&msg)?;

        // Block until the read_loop delivers the response.
        let response = rx
            .recv_timeout(std::time::Duration::from_secs(300))
            .context("prompt_sync_blocks: timeout waiting for response")?;

        let stop_reason = response
            .get("stopReason")
            .and_then(|v| v.as_str())
            .unwrap_or("end_turn")
            .to_string();

        // Broadcast complete state (unless cancelled — cancel sets it).
        {
            let mut state = self.prompt_turn_state.lock().unwrap();
            if !matches!(*state, PromptTurnState::Cancelled) {
                *state = PromptTurnState::Complete {
                    stop_reason: stop_reason.clone(),
                };
            }
        }
        self.broadcast_prompt_state();

        Ok(stop_reason)
    }

    /// True if the worker task loop should run.
    pub fn should_run_task_loop(&self) -> bool {
        let orch = self.orchestration.lock().unwrap();
        orch.caller_session_id.is_some()
            || orch.task_list.iter().any(|t| {
                t.status == super::orchestration::TaskStatus::Pending
                    || t.status == super::orchestration::TaskStatus::InProgress
            })
    }

    /// True if the orchestrator task loop should run.
    pub fn should_run_orchestrator_task_loop(&self) -> bool {
        let orch = self.orchestration.lock().unwrap();
        orch.orchestrator_task_list.iter().any(|t| {
            t.status == super::orchestration::OrchestratorTaskStatus::Pending
                || t.status == super::orchestration::OrchestratorTaskStatus::InProgress
                || t.status == super::orchestration::OrchestratorTaskStatus::Delegated
        })
    }

    /// Run the worker task loop: repeatedly prompt with the task list
    /// until all tasks are done or cancelled.
    pub fn run_task_loop(&self) -> Result<()> {
        if self
            .task_loop_running
            .swap(true, Ordering::SeqCst)
        {
            return Ok(());
        }

        loop {
            let decision = {
                let mut orch = self.orchestration.lock().unwrap();
                orch.determine_next_prompt()
            };

            match decision {
                Some(blocks) => {
                    self.broadcast_task_list();
                    let sr = self.prompt_sync_blocks(&blocks)?;
                    if sr == "cancelled" {
                        acp_log!(
                            "INFO",
                            "Task loop paused for session {} (cancelled)",
                            self.session_id()
                        );
                        break;
                    }
                }
                None => {
                    acp_log!(
                        "INFO",
                        "Task loop complete for session {}",
                        self.session_id()
                    );
                    break;
                }
            }
        }

        self.task_loop_running.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Run the orchestrator task loop.
    pub fn run_orchestrator_task_loop(&self) -> Result<()> {
        if self
            .orchestrator_task_loop_running
            .swap(true, Ordering::SeqCst)
        {
            return Ok(());
        }

        loop {
            let decision = {
                let mut orch = self.orchestration.lock().unwrap();
                orch.determine_next_orchestrator_prompt()
            };

            match decision {
                Some(blocks) => {
                    self.broadcast_orchestrator_task_list();
                    let sr = self.prompt_sync_blocks(&blocks)?;
                    if sr == "cancelled" {
                        break;
                    }
                }
                None => {
                    break;
                }
            }
        }

        self.orchestrator_task_loop_running
            .store(false, Ordering::SeqCst);
        Ok(())
    }

    // ─── Queue management ──────────────────────────────────────────────────

    pub fn queue_add(&self, blocks: Vec<Value>) {
        self.queue
            .lock()
            .unwrap()
            .push(QueueItem::Prompt(blocks));
        self.broadcast_queue_state();
    }

    pub fn queue_remove(&self, index: usize) -> Option<()> {
        let mut q = self.queue.lock().unwrap();
        if index < q.len() {
            q.remove(index);
            drop(q);
            self.broadcast_queue_state();
            Some(())
        } else {
            None
        }
    }

    pub fn queue_clear(&self) {
        self.queue.lock().unwrap().clear();
        self.broadcast_queue_state();
    }

    pub fn queue_list(&self) -> Vec<QueueItem> {
        self.queue.lock().unwrap().clone()
    }

    // ─── Broadcast helpers ─────────────────────────────────────────────────

    fn broadcast_queue_state(&self) {
        let items = self.queue.lock().unwrap().clone();
        let session_id = self.session_id();
        let update = json!({
            "sessionUpdate": "queue_changed",
            "items": items,
        });
        let _ = self.event_tx.send(SessionEvent::Update {
            session_id,
            update,
        });
    }

    fn broadcast_prompt_state(&self) {
        let state = self.prompt_turn_state.lock().unwrap().clone();
        let status = match &state {
            PromptTurnState::Running => "running",
            PromptTurnState::Idle => "idle",
            PromptTurnState::Cancelled => "idle",
            PromptTurnState::Complete { .. } => "idle",
            PromptTurnState::Error { .. } => "idle",
        };
        let session_id = self.session_id();
        let update = json!({
            "sessionUpdate": "prompt_state",
            "status": status,
        });
        let _ = self.event_tx.send(SessionEvent::Update {
            session_id,
            update,
        });
    }

    pub fn broadcast_task_list(&self) {
        let orch = self.orchestration.lock().unwrap();
        let tasks = orch.task_list.clone();
        drop(orch);
        let session_id = self.session_id();
        let update = json!({
            "sessionUpdate": "task_list_update",
            "tasks": tasks,
        });
        let _ = self.event_tx.send(SessionEvent::Update {
            session_id,
            update,
        });
    }

    pub fn broadcast_orchestrator_task_list(&self) {
        let orch = self.orchestration.lock().unwrap();
        let tasks = orch.orchestrator_task_list.clone();
        drop(orch);
        let session_id = self.session_id();
        let update = json!({
            "sessionUpdate": "orchestrator_task_list_update",
            "tasks": tasks,
        });
        let _ = self.event_tx.send(SessionEvent::Update {
            session_id,
            update,
        });
    }

    /// If this session has a registered caller and all tasks are done,
    /// notify the caller. Called after each prompt turn completes.
    pub fn notify_if_done(&self) {
        let caller_sid = {
            let orch = self.orchestration.lock().unwrap();
            let all_done = orch.task_list.iter().all(|t| {
                t.status == super::orchestration::TaskStatus::Completed
                    || t.status == super::orchestration::TaskStatus::Failed
                    || t.status == super::orchestration::TaskStatus::Cancelled
            });
            if !all_done || orch.caller_session_id.is_none() {
                return;
            }
            orch.caller_session_id.clone()
        };

        // Take the caller so we only notify once.
        let caller_sid = match caller_sid {
            Some(sid) => sid,
            None => return,
        };
        {
            let mut orch = self.orchestration.lock().unwrap();
            orch.caller_session_id = None;
        }

        let worker_sid = self.session_id();
        let text = format!(
            "Session {} has completed its task list. \
             Call query_memory(session_id=\"{}\", limit=1) to see the agent's last message.",
            worker_sid, worker_sid
        );

        // Send the notification to the caller via the proxy RPC.
        self.proxy_rpc.notification(ProxyNotification::AcpPromptCallback {
            target_session_id: caller_sid,
            text,
        });
    }

    /// List available sessions.
    pub fn list_sessions(&self, cwd: &str) -> Result<Value> {
        let params = json!({ "cwd": cwd });
        self.request("session/list", params)
    }

    /// Send a JSON-RPC request and wait for the response (blocking).
    fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = bounded(1);
        self.pending.lock().unwrap().insert(id, tx);

        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.send_raw(&msg)?;

        // Wait for response with a timeout.
        match rx.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(err)) => Err(anyhow::anyhow!("RPC error: {}", err)),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(anyhow::anyhow!("RPC timeout for {}", method))
            }
        }
    }

    /// Send a raw JSON value to the agent's stdin.
    fn send_raw(&self, msg: &Value) -> Result<()> {
        let line = serde_json::to_string(msg)?;
        acp_log!(
            "SEND",
            "connection={} json={}",
            self.connection_id,
            line
        );
        self.agent_manager.send(&self.agent_id, &line)
    }

    /// Send a JSON-RPC response back to the agent (for tool requests).
    pub fn send_tool_response(&self, rpc_id: &Value, result: Value) -> Result<()> {
        let msg = json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "result": result,
        });
        self.send_raw(&msg)
    }

    /// Send a JSON-RPC *error* back to the agent (for tool requests we can't
    /// or won't service). Without this an unsupported client tool would hang
    /// the agent forever waiting on a response.
    pub fn send_tool_error(
        &self,
        rpc_id: &Value,
        code: i64,
        message: &str,
    ) -> Result<()> {
        let msg = json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "error": { "code": code, "message": message },
        });
        self.send_raw(&msg)
    }

    /// The main read loop — processes JSON-RPC messages from agent stdout.
    fn read_loop(&self, stdout_rx: &Receiver<String>) {
        // Tool requests are routed to the Dispatcher, which replies directly
        // via `send_tool_response`, so the reader only drains agent stdout.
        while let Ok(line) = stdout_rx.recv() {
            self.handle_line(&line);
        }

        // Agent disconnected.
        let session_id = self.session_id();
        acp_log!(
            "INFO",
            "connection={} agent stdout closed, session={}",
            self.connection_id,
            session_id
        );
        let _ = self.event_tx.send(SessionEvent::Disconnected { session_id });
    }

    /// Handle a single JSON-RPC line from the agent.
    fn handle_line(&self, line: &str) {
        acp_log!(
            "RECV",
            "connection={} line={}",
            self.connection_id,
            line
        );

        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                acp_log!(
                    "ERROR",
                    "connection={} non-JSON line from agent: {} (error: {})",
                    self.connection_id,
                    line,
                    e
                );
                return;
            }
        };

        // Response to one of our requests.
        if let Some(id) = msg.get("id") {
            if msg.get("result").is_some() || msg.get("error").is_some() {
                if let Some(id_num) = id.as_u64() {
                    if let Some(tx) = self.pending.lock().unwrap().remove(&id_num) {
                        if let Some(result) = msg.get("result") {
                            let _ = tx.send(Ok(result.clone()));
                        } else if let Some(error) = msg.get("error") {
                            let _ = tx.send(Err(error.clone()));
                        }
                        return;
                    }

                    // The fire-and-forget `session/prompt` response is the
                    // canonical end-of-turn signal (carries `stopReason`).
                    // Mirror crow-acp's `broadcast_prompt_state(Complete)`:
                    // synthesize a `prompt_complete` update onto the same
                    // stream the UI already consumes, so the frontend clears
                    // its loading state off one uniform channel.
                    let mut active = self.active_prompt_id.lock().unwrap();
                    if *active == Some(id_num) {
                        *active = None;
                        let stop_reason = msg
                            .get("result")
                            .and_then(|r| r.get("stopReason"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("end_turn");
                        let session_id = self.session_id();
                        let update = json!({
                            "sessionUpdate": "prompt_complete",
                            "stopReason": stop_reason,
                        });
                        let _ = self.event_tx.send(SessionEvent::Update {
                            session_id,
                            update,
                        });

                        // Unblock prompt_sync_blocks if waiting.
                        if let Some(tx) =
                            self.prompt_sync_tx.lock().unwrap().take()
                        {
                            let result = msg
                                .get("result")
                                .cloned()
                                .unwrap_or(json!({}));
                            let _ = tx.send(result);
                        }
                    }
                }

                // Response to a tool request we sent — ignore (already handled).
                return;
            }
        }

        // Request from agent (tool call).
        if let (Some(id), Some(method)) = (msg.get("id"), msg.get("method")) {
            if let Some(method_str) = method.as_str() {
                let params = msg
                    .get("params")
                    .cloned()
                    .unwrap_or(Value::Null);

                // Route the client-tool request onto the Dispatcher's loop as
                // a synthetic ProxyNotification, handled there with `&mut self`
                // (access to the open document model). No detached thread, no
                // fake permission prompt — tools just execute.
                self.proxy_rpc.notification(
                    ProxyNotification::AcpClientTool {
                        session_id: self.session_id(),
                        rpc_id: id.clone(),
                        method: method_str.to_string(),
                        params,
                    },
                );
                return;
            }
        }

        // Notification from agent.
        if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
            match method {
                "session/update" => {
                    let session_id = self.session_id();
                    // ACP `session/update` params are a `SessionNotification`
                    // envelope `{ sessionId, update }`. Forward the *inner*
                    // `update` (the discriminated `SessionUpdate`), exactly as
                    // crow-acp's `handle_agent_line` does, so the UI sees
                    // `sessionUpdate` at the top level.
                    let params = msg.get("params");
                    let update = params
                        .and_then(|p| p.get("update"))
                        .cloned()
                        .or_else(|| params.cloned())
                        .unwrap_or(Value::Null);
                    let _ = self.event_tx.send(SessionEvent::Update {
                        session_id,
                        update,
                    });
                }
                _ => {
                    acp_log!("WARN", "connection={} unhandled ACP notification: {}", self.connection_id, method);
                }
            }
        }
    }
}

/// Manager for multiple ACP sessions.
pub struct AcpSessionManager {
    sessions: Mutex<HashMap<String, Arc<AcpSession>>>,
    agent_manager: Arc<AgentManager>,
}

impl AcpSessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            agent_manager: Arc::new(AgentManager::new()),
        }
    }

    /// Spawn + initialize + create a new session in one shot.
    pub fn create_session(
        &self,
        config: AgentConfig,
        cwd: &str,
        mcp_servers: Vec<Value>,
        event_tx: Sender<SessionEvent>,
        proxy_rpc: ProxyRpcHandler,
        load_session_id: Option<String>,
    ) -> Result<Arc<AcpSession>> {
        acp_log!(
            "INFO",
            "Creating session: agent={}, cwd={}, load={:?}",
            config.name,
            cwd,
            load_session_id
        );
        let session = AcpSession::spawn(
            &self.agent_manager,
            config,
            cwd,
            event_tx,
            proxy_rpc,
        )?;
        acp_log!("INFO", "connection={} sending initialize", session.connection_id);
        session.initialize()?;
        match load_session_id {
            Some(target) => {
                acp_log!(
                    "INFO",
                    "connection={} sending session/load (resume) target={}",
                    session.connection_id,
                    target
                );
                session.load_session(&target, cwd)?;
            }
            None => {
                acp_log!("INFO", "connection={} sending session/new", session.connection_id);
                session.new_session(mcp_servers)?;
            }
        }
        let session_id = session.session_id();
        acp_log!("INFO", "ACP session ready: {}", session_id);
        self.sessions
            .lock()
            .unwrap()
            .insert(session_id, session.clone());
        Ok(session)
    }

    pub fn get_session(&self, session_id: &str) -> Option<Arc<AcpSession>> {
        self.sessions.lock().unwrap().get(session_id).cloned()
    }

    pub fn close_session(&self, session_id: &str) {
        if let Some(session) = self.sessions.lock().unwrap().remove(session_id) {
            self.agent_manager.kill(&session.agent_id);
        }
    }

    pub fn list_sessions(&self) -> Vec<String> {
        self.sessions.lock().unwrap().keys().cloned().collect()
    }
}
