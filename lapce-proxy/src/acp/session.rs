//! ACP session — speaks JSON-RPC over the agent's stdio.
//!
//! Ported from crow-acp's session.rs, adapted to sync threads + crossbeam.
//! Handles the ACP protocol lifecycle: initialize → session/new → prompt.
//! Client tool requests (fs, terminal) are forwarded to the Dispatcher.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, bounded};
use serde_json::{Value, json};

use super::agent::{AgentConfig, AgentManager};
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
            agent_config: config,
            agent_manager: agent_manager.clone(),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            active_prompt_id: Mutex::new(None),
            event_tx,
            cwd: cwd.to_string(),
            proxy_rpc,
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
        tracing::debug!(
            conn = %self.connection_id,
            msg = %line,
            "ACP >> agent"
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
        tracing::info!(
            conn = %self.connection_id,
            session = %session_id,
            "ACP: agent disconnected"
        );
        let _ = self.event_tx.send(SessionEvent::Disconnected { session_id });
    }

    /// Handle a single JSON-RPC line from the agent.
    fn handle_line(&self, line: &str) {
        tracing::debug!(
            conn = %self.connection_id,
            msg = %line,
            "ACP << agent"
        );

        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    conn = %self.connection_id,
                    line = %line,
                    error = %e,
                    "ACP: non-JSON line from agent, ignoring"
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
                    tracing::debug!("Unhandled ACP notification: {}", method);
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
    ) -> Result<Arc<AcpSession>> {
        tracing::info!(
            agent = %config.name,
            cwd = %cwd,
            "ACP: creating session"
        );
        let session = AcpSession::spawn(
            &self.agent_manager,
            config,
            cwd,
            event_tx,
            proxy_rpc,
        )?;
        tracing::info!(conn = %session.connection_id, "ACP: sending initialize");
        session.initialize()?;
        tracing::info!(conn = %session.connection_id, "ACP: sending session/new");
        session.new_session(mcp_servers)?;
        let session_id = session.session_id();
        tracing::info!(session = %session_id, "ACP: session created");
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
