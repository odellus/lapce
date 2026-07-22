//! Agent process management — spawn/kill ACP agent subprocesses.
//!
//! Ported from crow-acp's agent.rs, adapted from tokio to std threads
//! to match lapce-proxy's synchronous architecture.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;

use anyhow::{Context, Result};
use crossbeam_channel::{Receiver, Sender, bounded};
use serde::{Deserialize, Serialize};

/// Configuration for an ACP agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Agent name (e.g. "claude-code", "gemini-cli")
    pub name: String,
    /// Command to spawn the agent
    pub command: String,
    /// Arguments to pass to the command
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables (key=value pairs)
    #[serde(default)]
    pub env: Vec<String>,
}

/// A running agent subprocess.
struct AgentInstance {
    process: Child,
    /// Send JSON-RPC lines to agent stdin.
    stdin_tx: Sender<String>,
}

/// Receives raw stdout lines from an agent.
pub type AgentStdoutRx = Receiver<String>;

/// Manages spawned agent subprocesses.
pub struct AgentManager {
    agents: Mutex<HashMap<String, AgentInstance>>,
    stdout_receivers: Mutex<HashMap<String, AgentStdoutRx>>,
    next_id: AtomicU64,
}

impl AgentManager {
    pub fn new() -> Self {
        Self {
            agents: Mutex::new(HashMap::new()),
            stdout_receivers: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Spawn an agent subprocess connected via JSON-RPC over stdio.
    /// Returns the agent ID. Stdout lines are available via `take_stdout_rx`.
    pub fn spawn(&self, config: &AgentConfig, cwd: &str) -> Result<String> {
        let id = format!("agent_{}", self.next_id.fetch_add(1, Ordering::Relaxed));

        tracing::info!(
            agent = %config.name,
            command = %config.command,
            args = ?config.args,
            cwd = %cwd,
            "ACP: spawning agent"
        );

        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Apply user env overrides.
        for env in &config.env {
            if let Some((k, v)) = env.split_once('=') {
                cmd.env(k, v);
            }
        }

        let mut process = cmd
            .spawn()
            .with_context(|| {
                let err = format!(
                    "Failed to spawn agent '{}' (command='{}', cwd='{}')",
                    config.name, config.command, cwd
                );
                tracing::error!(%err);
                err
            })?;

        let mut stdin = process.stdin.take().context("No stdin")?;
        let stdout = process.stdout.take().context("No stdout")?;
        let stderr = process.stderr.take().context("No stderr")?;

        // Channel for sending JSON-RPC lines to agent stdin.
        let (stdin_tx, stdin_rx) = bounded::<String>(1024);

        // Thread: pump messages from channel → agent stdin.
        thread::Builder::new()
            .name(format!("acp-stdin-{}", id))
            .spawn(move || {
                while let Ok(msg) = stdin_rx.recv() {
                    if writeln!(stdin, "{}", msg).is_err() {
                        break;
                    }
                }
            })?;

        // Channel for receiving stdout lines from agent.
        let (stdout_tx, stdout_rx) = bounded::<String>(4096);

        // Thread: pump lines from agent stdout → channel.
        let id_out = id.clone();
        thread::Builder::new()
            .name(format!("acp-stdout-{}", id))
            .spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    match line {
                        Ok(line) => {
                            let trimmed = line.trim().to_string();
                            if !trimmed.is_empty() && stdout_tx.send(trimmed).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                tracing::info!("Agent {} stdout reader exited", id_out);
            })?;

        // Thread: drain stderr → tracing logs.
        let id_err = id.clone();
        thread::Builder::new()
            .name(format!("acp-stderr-{}", id))
            .spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines() {
                    match line {
                        Ok(line) => {
                            let trimmed = line.trim();
                            if !trimmed.is_empty() {
                                tracing::warn!("[{} stderr] {}", id_err, trimmed);
                            }
                        }
                        Err(_) => break,
                    }
                }
            })?;

        let instance = AgentInstance { process, stdin_tx };

        self.agents.lock().unwrap().insert(id.clone(), instance);
        self.stdout_receivers.lock().unwrap().insert(id.clone(), stdout_rx);

        tracing::info!("Agent spawned: {} (id={})", config.name, id);
        Ok(id)
    }

    /// Send a JSON-RPC line to an agent's stdin.
    pub fn send(&self, agent_id: &str, msg: &str) -> Result<()> {
        let agents = self.agents.lock().unwrap();
        let instance = agents
            .get(agent_id)
            .with_context(|| format!("Agent not found: {}", agent_id))?;
        instance
            .stdin_tx
            .send(msg.to_string())
            .map_err(|_| anyhow::anyhow!("Agent stdin channel closed"))
    }

    /// Take the stdout receiver for an agent (can only be called once).
    pub fn take_stdout_rx(&self, agent_id: &str) -> Option<AgentStdoutRx> {
        self.stdout_receivers.lock().unwrap().remove(agent_id)
    }

    /// Kill an agent process and remove it.
    pub fn kill(&self, agent_id: &str) {
        if let Some(mut instance) = self.agents.lock().unwrap().remove(agent_id) {
            tracing::info!("Killing agent {}", agent_id);
            let _ = instance.process.kill();
            let _ = instance.process.wait();
        }
        self.stdout_receivers.lock().unwrap().remove(agent_id);
    }
}
