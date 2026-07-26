//! Integration tests for ACP orchestration using fake Python agents.
//!
//! These tests spawn real Python subprocesses that speak ACP over stdio,
//! exercising the full prompt → queue → task-loop → callback pipeline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver};
use lapce_proxy::acp::agent::AgentManager;
use lapce_proxy::acp::orchestration::{
    OrchestratorTask, OrchestratorTaskStatus, Task, TaskStatus,
};
use lapce_proxy::acp::session::{AcpSession, SessionEvent};
use lapce_rpc::proxy::{ProxyNotification, ProxyRpc, ProxyRpcHandler};
use serde_json::{json, Value};

fn test_cwd() -> String {
    std::env::temp_dir().to_string_lossy().to_string()
}

fn manifest_path(name: &str) -> String {
    std::env::current_dir()
        .unwrap()
        .join("tests")
        .join(name)
        .to_string_lossy()
        .to_string()
}

fn echo_agent_config() -> lapce_proxy::acp::agent::AgentConfig {
    lapce_proxy::acp::agent::AgentConfig {
        name: "echo".to_string(),
        command: "python3".to_string(),
        args: vec![manifest_path("echo_agent.py")],
        env: vec![],
    }
}

fn orchestration_agent_config(role: &str, worker: &str) -> lapce_proxy::acp::agent::AgentConfig {
    let mut args = vec![
        manifest_path("orchestration_agent.py"),
        "--role".to_string(),
        role.to_string(),
    ];
    if !worker.is_empty() {
        args.push("--worker".to_string());
        args.push(worker.to_string());
    }
    lapce_proxy::acp::agent::AgentConfig {
        name: format!("orch-{}", role),
        command: "python3".to_string(),
        args,
        env: vec![],
    }
}

/// Spawn an agent, initialize, and create a session.
fn setup_session(
    config: &lapce_proxy::acp::agent::AgentConfig,
) -> (Arc<AcpSession>, Receiver<SessionEvent>, ProxyRpcHandler) {
    let rpc = ProxyRpcHandler::new();
    let agents = Arc::new(AgentManager::new());
    let (event_tx, event_rx) = unbounded();
    let session = AcpSession::spawn(
        &agents,
        config.clone(),
        &test_cwd(),
        event_tx,
        rpc.clone(),
    )
    .expect("agent spawn failed");
    session.initialize().expect("initialize failed");
    session.new_session(vec![]).expect("new_session failed");
    (session, event_rx, rpc)
}

/// Collect SessionEvent::Update values for a given session within a timeout.
fn collect_updates(
    event_rx: &Receiver<SessionEvent>,
    timeout: Duration,
) -> Vec<Value> {
    let mut updates = Vec::new();
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match event_rx.recv_timeout(remaining) {
            Ok(SessionEvent::Update { update, .. }) => {
                updates.push(update);
            }
            Ok(SessionEvent::Disconnected { .. }) => break,
            Err(_) => break,
        }
    }
    updates
}

/// Handle AcpClientTool notifications from the rpc channel.
/// This replicates the dispatch layer's handle_acp_tool for orchestration tools.
fn handle_tool_calls(
    rpc: &ProxyRpcHandler,
    sessions: &std::collections::HashMap<String, Arc<AcpSession>>,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match rpc.rx().recv_timeout(remaining) {
            Ok(ProxyRpc::Notification(ProxyNotification::AcpClientTool {
                session_id,
                rpc_id,
                method,
                params,
            })) => {
                let session = match sessions.get(&session_id) {
                    Some(s) => s.clone(),
                    None => continue,
                };
                handle_orchestration_tool(&session, &rpc_id, &method, params, sessions);
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
}

fn handle_orchestration_tool(
    session: &AcpSession,
    rpc_id: &Value,
    method: &str,
    params: Value,
    sessions: &std::collections::HashMap<String, Arc<AcpSession>>,
) {
    match method {
        "_task/read" | "task_read" => {
            let orch = session.orchestration.lock().unwrap();
            let tasks = orch.task_list.clone();
            drop(orch);
            let _ = session.send_tool_response(
                rpc_id,
                json!({ "tasks": tasks, "summary": format!("{:?}", tasks) }),
            );
        }
        "_task/write" | "task_write" => {
            let todos = params
                .get("todos")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let now = chrono::Utc::now();
            let tasks: Vec<Task> = todos
                .iter()
                .enumerate()
                .map(|(i, todo)| Task {
                    id: i.to_string(),
                    title: todo
                        .get("content")
                        .or_else(|| todo.get("title"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(&format!("Task {}", i + 1))
                        .to_string(),
                    description: None,
                    status: todo
                        .get("status")
                        .and_then(|v| v.as_str())
                        .map(|s| match s {
                            "completed" => TaskStatus::Completed,
                            "failed" => TaskStatus::Failed,
                            "cancelled" => TaskStatus::Cancelled,
                            "in_progress" => TaskStatus::InProgress,
                            _ => TaskStatus::Pending,
                        })
                        .unwrap_or(TaskStatus::Pending),
                    priority: todo
                        .get("priority")
                        .and_then(|v| v.as_str())
                        .unwrap_or("medium")
                        .to_string(),
                    assigned_to: todo
                        .get("assignedTo")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    created_at: now,
                    updated_at: now,
                })
                .collect();
            {
                let mut orch = session.orchestration.lock().unwrap();
                orch.task_list = tasks.clone();
            }
            session.broadcast_task_list();
            let _ = session.send_tool_response(rpc_id, json!({ "tasks": tasks }));
        }
        "_task/orchestrator/read" | "orchestrator_task_read" => {
            let orch = session.orchestration.lock().unwrap();
            let tasks = orch.orchestrator_task_list.clone();
            drop(orch);
            let _ = session.send_tool_response(
                rpc_id,
                json!({ "tasks": tasks, "summary": format!("{:?}", tasks) }),
            );
        }
        "_task/orchestrator/write" | "orchestrator_task_write" => {
            let todos = params
                .get("todos")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let now = chrono::Utc::now();
            let tasks: Vec<OrchestratorTask> = todos
                .iter()
                .enumerate()
                .map(|(i, todo)| OrchestratorTask {
                    id: i.to_string(),
                    title: todo
                        .get("content")
                        .or_else(|| todo.get("title"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(&format!("Task {}", i + 1))
                        .to_string(),
                    description: None,
                    status: todo
                        .get("status")
                        .and_then(|v| v.as_str())
                        .map(|s| match s {
                            "completed" => OrchestratorTaskStatus::Completed,
                            "failed" => OrchestratorTaskStatus::Failed,
                            "cancelled" => OrchestratorTaskStatus::Cancelled,
                            "in_progress" => OrchestratorTaskStatus::InProgress,
                            "delegated" => OrchestratorTaskStatus::Delegated,
                            _ => OrchestratorTaskStatus::Pending,
                        })
                        .unwrap_or(OrchestratorTaskStatus::Pending),
                    priority: todo
                        .get("priority")
                        .and_then(|v| v.as_str())
                        .unwrap_or("medium")
                        .to_string(),
                    assigned_to: todo
                        .get("assignedTo")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    created_at: now,
                    updated_at: now,
                })
                .collect();
            {
                let mut orch = session.orchestration.lock().unwrap();
                orch.orchestrator_task_list = tasks.clone();
            }
            session.broadcast_orchestrator_task_list();
            let _ = session.send_tool_response(rpc_id, json!({ "tasks": tasks }));
        }
        "_task/send" | "task_send" => {
            let to_sid = params
                .get("toSessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let task_defs = params
                .get("tasks")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            match sessions.get(to_sid) {
                Some(target_session) => {
                    let now = chrono::Utc::now();
                    let tasks: Vec<Task> = task_defs
                        .iter()
                        .enumerate()
                        .filter_map(|(i, def)| {
                            let title = def.get("title")?.as_str()?.to_string();
                            Some(Task {
                                id: i.to_string(),
                                title,
                                description: def
                                    .get("description")
                                    .and_then(|v| v.as_str())
                                    .map(String::from),
                                status: TaskStatus::Pending,
                                priority: "medium".to_string(),
                                assigned_to: None,
                                created_at: now,
                                updated_at: now,
                            })
                        })
                        .collect();

                    let caller_sid = session.session_id();
                    {
                        let mut orch = target_session.orchestration.lock().unwrap();
                        orch.task_list = tasks.clone();
                        orch.set_caller(caller_sid);
                    }
                    target_session.broadcast_task_list();

                    let _ = session.send_tool_response(
                        rpc_id,
                        json!({
                            "success": true,
                            "taskCount": tasks.len(),
                            "toSessionId": to_sid,
                        }),
                    );

                    // Start the target's task loop.
                    let target = target_session.clone();
                    std::thread::Builder::new()
                        .name("test-task-loop".to_string())
                        .spawn(move || {
                            if let Err(e) = target.run_task_loop() {
                                eprintln!("task loop failed: {}", e);
                            }
                            target.notify_if_done();
                        })
                        .ok();
                }
                None => {
                    let _ = session.send_tool_error(
                        rpc_id,
                        -32000,
                        &format!("target session not found: {}", to_sid),
                    );
                }
            }
        }
        "_send" | "send_to_session" => {
            let to_sid = params
                .get("toSessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let blocks = params
                .get("blocks")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            match sessions.get(to_sid) {
                Some(target_session) => {
                    let _ = session.send_tool_response(
                        rpc_id,
                        json!({ "success": true }),
                    );
                    let target = target_session.clone();
                    std::thread::Builder::new()
                        .name("test-send-prompt".to_string())
                        .spawn(move || {
                            let _ = target.prompt_with_queue(blocks);
                        })
                        .ok();
                }
                None => {
                    let _ = session.send_tool_error(
                        rpc_id,
                        -32000,
                        &format!("target session not found: {}", to_sid),
                    );
                }
            }
        }
        _ => {
            // Unknown tool — respond with empty result
            let _ = session.send_tool_response(rpc_id, json!({}));
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[test]
fn test_echo_agent_basic_prompt() {
    let (session, event_rx, _rpc) = setup_session(&echo_agent_config());

    // Use prompt_async (fire-and-forget) so handle_line synthesizes
    // prompt_complete when the response arrives.
    let result = session.prompt_async("Hello world");
    assert!(result.is_ok(), "prompt failed: {:?}", result.err());

    // Collect events — echo agent sends agent_message_chunk + prompt_complete
    let updates = collect_updates(&event_rx, Duration::from_secs(5));

    let has_chunk = updates.iter().any(|u| {
        u.get("sessionUpdate").and_then(|v| v.as_str())
            == Some("agent_message_chunk")
    });
    assert!(
        has_chunk,
        "expected agent_message_chunk, got: {:?}",
        updates
    );

    // prompt_complete is synthesized by handle_line for async prompts
    let has_complete = updates.iter().any(|u| {
        u.get("sessionUpdate").and_then(|v| v.as_str())
            == Some("prompt_complete")
    });
    assert!(
        has_complete,
        "expected prompt_complete, got: {:?}",
        updates
    );
}

#[test]
fn test_prompt_queue_serializes() {
    let (session, event_rx, _rpc) = setup_session(&echo_agent_config());

    // Send two prompts simultaneously via prompt_with_queue.
    // With a fast echo agent, the first may finish before the second starts,
    // so we can't guarantee queue_changed appears. What we CAN guarantee is
    // that both prompts produce output (no lost prompts, no deadlock).
    let s1 = session.clone();
    let h1 = std::thread::spawn(move || s1.prompt_with_queue(
        vec![json!({"type": "text", "text": "First"})],
    ));
    let s2 = session.clone();
    let h2 = std::thread::spawn(move || s2.prompt_with_queue(
        vec![json!({"type": "text", "text": "Second"})],
    ));

    h1.join().unwrap().expect("first prompt failed");
    h2.join().unwrap().expect("second prompt failed");

    // Collect events
    let updates = collect_updates(&event_rx, Duration::from_secs(5));

    // Both prompts must have produced agent_message_chunks
    let chunk_count = updates
        .iter()
        .filter(|u| {
            u.get("sessionUpdate").and_then(|v| v.as_str())
                == Some("agent_message_chunk")
        })
        .count();
    assert!(
        chunk_count >= 2,
        "expected >= 2 agent_message_chunks (one per prompt), got {}",
        chunk_count
    );

    // Both prompts must have completed
    let complete_count = updates
        .iter()
        .filter(|u| {
            u.get("sessionUpdate").and_then(|v| v.as_str())
                == Some("prompt_complete")
        })
        .count();
    assert!(
        complete_count >= 2,
        "expected >= 2 prompt_complete events, got {}",
        complete_count
    );
}

#[test]
fn test_orchestration_worker_completes_task() {
    // Set up a worker session
    let (worker_session, worker_rx, worker_rpc) =
        setup_session(&orchestration_agent_config("worker", ""));
    let worker_sid = worker_session.session_id();

    // Pre-populate the worker's task list (simulating what _task/send does)
    {
        let now = chrono::Utc::now();
        let mut orch = worker_session.orchestration.lock().unwrap();
        orch.task_list = vec![
            Task {
                id: "0".to_string(),
                title: "E2E task one".to_string(),
                description: Some("First scripted task.".to_string()),
                status: TaskStatus::Pending,
                priority: "medium".to_string(),
                assigned_to: None,
                created_at: now,
                updated_at: now,
            },
            Task {
                id: "1".to_string(),
                title: "E2E task two".to_string(),
                description: Some("Second scripted task.".to_string()),
                status: TaskStatus::Pending,
                priority: "medium".to_string(),
                assigned_to: None,
                created_at: now,
                updated_at: now,
            },
        ];
        orch.set_caller("test-caller".to_string());
    }
    worker_session.broadcast_task_list();

    // Build a session map for the tool handler
    let mut sessions = std::collections::HashMap::new();
    sessions.insert(worker_sid.clone(), worker_session.clone());

    // Start tool handler thread
    let sessions_for_tools = sessions.clone();
    let tool_handle = std::thread::spawn(move || {
        handle_tool_calls(&worker_rpc, &sessions_for_tools, Duration::from_secs(10));
    });

    // Run the task loop — this will prompt the worker agent, which will
    // call _task/read + _task/write to mark the first task completed,
    // then the loop continues for the second task.
    let ws = worker_session.clone();
    let loop_handle = std::thread::spawn(move || {
        ws.run_task_loop().expect("task loop failed");
        ws.notify_if_done();
    });

    loop_handle.join().unwrap();

    // Collect events from the worker
    let updates = collect_updates(&worker_rx, Duration::from_secs(3));

    // Should see task_list_update events
    let task_updates: Vec<_> = updates
        .iter()
        .filter(|u| {
            u.get("sessionUpdate").and_then(|v| v.as_str())
                == Some("task_list_update")
        })
        .collect();
    assert!(
        !task_updates.is_empty(),
        "expected task_list_update events, got: {:?}",
        updates.iter()
            .map(|u| u.get("sessionUpdate").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
    );

    // Verify final task state: both tasks should be completed
    let orch = worker_session.orchestration.lock().unwrap();
    assert_eq!(orch.task_list.len(), 2);
    assert_eq!(orch.task_list[0].status, TaskStatus::Completed);
    assert_eq!(orch.task_list[1].status, TaskStatus::Completed);
    drop(orch);

    // The tool handler thread will time out — that's fine
    drop(tool_handle);
}

#[test]
fn test_orchestration_task_send_between_sessions() {
    // Set up orchestrator + worker sessions
    let (worker_session, _worker_rx, worker_rpc) =
        setup_session(&orchestration_agent_config("worker", ""));
    let worker_sid = worker_session.session_id();

    let (orch_session, orch_rx, orch_rpc) =
        setup_session(&orchestration_agent_config("orchestrator", &worker_sid));
    let orch_sid = orch_session.session_id();

    // Build session map for tool handlers
    let mut sessions = std::collections::HashMap::new();
    sessions.insert(worker_sid.clone(), worker_session.clone());
    sessions.insert(orch_sid.clone(), orch_session.clone());

    // Tool handler for orchestrator's tools (_task/send)
    let sessions_for_orch = sessions.clone();
    let orch_tool_handle = std::thread::spawn(move || {
        handle_tool_calls(&orch_rpc, &sessions_for_orch, Duration::from_secs(10));
    });

    // Tool handler for worker's tools (_task/read, _task/write)
    let sessions_for_worker = sessions.clone();
    let worker_tool_handle = std::thread::spawn(move || {
        handle_tool_calls(&worker_rpc, &sessions_for_worker, Duration::from_secs(10));
    });

    // Prompt the orchestrator — it will call _task/send to the worker
    let result = orch_session.prompt("Start the work");
    assert!(result.is_ok(), "orchestrator prompt failed: {:?}", result.err());

    // Wait for the orchestrator's prompt to complete and the task loop to run
    std::thread::sleep(Duration::from_secs(3));

    // Collect orchestrator events
    let orch_updates = collect_updates(&orch_rx, Duration::from_secs(2));

    // The orchestrator should have sent an agent_message_chunk
    let has_orch_msg = orch_updates.iter().any(|u| {
        u.get("sessionUpdate").and_then(|v| v.as_str())
            == Some("agent_message_chunk")
    });
    assert!(
        has_orch_msg,
        "expected orchestrator message, got: {:?}",
        orch_updates.iter()
            .map(|u| u.get("sessionUpdate").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
    );

    // The worker's task list should have been populated and tasks completed
    let orch = worker_session.orchestration.lock().unwrap();
    assert!(
        !orch.task_list.is_empty(),
        "worker task list should not be empty"
    );
    // After the task loop runs, tasks should be completed
    let all_done = orch.task_list.iter().all(|t| t.status == TaskStatus::Completed);
    assert!(
        all_done,
        "expected all worker tasks completed, got: {:?}",
        orch.task_list
    );
    drop(orch);

    drop(orch_tool_handle);
    drop(worker_tool_handle);
}
