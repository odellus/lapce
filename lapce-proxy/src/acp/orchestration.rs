//! Orchestration types and pure state machine — ported from crow-ade.
//!
//! No I/O, no async, fully unit-testable. The decision core of the task loop.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

// ─── Prompt turn lifecycle ─────────────────────────────────────────────────

/// Lifecycle state of a prompt turn, owned by the backend.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum PromptTurnState {
    #[default]
    Idle,
    Running,
    Complete {
        stop_reason: String,
    },
    Cancelled,
    Error {
        message: String,
    },
}

// ─── Task types ────────────────────────────────────────────────────────────

/// A task in the worker's task list.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: TaskStatus,
    pub priority: String,
    pub assigned_to: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Task execution status.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

/// A task in the orchestrator's task list.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrchestratorTask {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: OrchestratorTaskStatus,
    pub priority: String,
    pub assigned_to: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Orchestrator task execution status.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratorTaskStatus {
    Pending,
    InProgress,
    Delegated,
    Completed,
    Failed,
    Cancelled,
}

/// A single item in the session's prompt queue.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueItem {
    Prompt(Vec<Value>),
    Task(Task),
}

// ─── Pure orchestration state machine ──────────────────────────────────────

/// Pure orchestration state — the testable core of the task loop.
#[derive(Debug, Default)]
pub struct OrchestrationState {
    /// The plan / TODO — single source of truth for task status.
    pub task_list: Vec<Task>,
    /// The orchestrator's own task list, with delegation support.
    pub orchestrator_task_list: Vec<OrchestratorTask>,
    /// Who sent this task list (set by `task_send`). When the loop exits
    /// with all tasks done, a "done" notification is sent to this session.
    pub caller_session_id: Option<String>,
}

impl OrchestrationState {
    /// Decide what to prompt the agent with next.
    ///
    /// Read-only: does NOT mutate the task list. The agent owns all status
    /// transitions via `task_write`. If any task is still incomplete (Pending
    /// or InProgress), returns the full task list as a prompt so the agent
    /// sees what's left. If everything is done, returns None (loop exits,
    /// caller is notified).
    pub fn determine_next_prompt(&mut self) -> Option<Vec<Value>> {
        let has_incomplete = self.task_list.iter().any(|t| {
            t.status == TaskStatus::Pending || t.status == TaskStatus::InProgress
        });

        if has_incomplete {
            Some(self.task_list_prompt())
        } else {
            None
        }
    }

    /// Decide what to prompt the orchestrator with next.
    ///
    /// Read-only: does NOT mutate the task list. If any task is still
    /// incomplete (Pending or InProgress), returns the full task list as a
    /// prompt. If the first non-completed task is Delegated, the orchestrator
    /// is waiting on a worker and should not be nagged, so returns None.
    pub fn determine_next_orchestrator_prompt(&mut self) -> Option<Vec<Value>> {
        let active = self.orchestrator_task_list.iter().find(|t| {
            t.status == OrchestratorTaskStatus::Pending
                || t.status == OrchestratorTaskStatus::InProgress
                || t.status == OrchestratorTaskStatus::Delegated
        });

        match active {
            Some(t) if t.status == OrchestratorTaskStatus::Delegated => None,
            Some(_) => Some(self.orchestrator_task_list_prompt()),
            None => None,
        }
    }

    /// Called by `task_send` to record who sent this task list.
    pub fn set_caller(&mut self, session_id: String) {
        self.caller_session_id = Some(session_id);
    }

    fn task_list_prompt(&self) -> Vec<Value> {
        let task_lines: Vec<String> = self
            .task_list
            .iter()
            .map(|t| {
                let status = match t.status {
                    TaskStatus::InProgress => "in_progress",
                    TaskStatus::Pending => "pending",
                    TaskStatus::Completed => "completed",
                    TaskStatus::Failed => "failed",
                    TaskStatus::Cancelled => "cancelled",
                };
                format!("- [{}] {}", status, t.title)
            })
            .collect();

        vec![json!({
            "type": "text",
            "text": format!(
                "SYSTEM MESSAGE:\n\nTask list:\n\n{}\n\n\
                 Call task_read if you need to check the current state. \
                 Complete all unfinished tasks. Call task_write to update statuses.",
                task_lines.join("\n"),
            )
        })]
    }

    fn orchestrator_task_list_prompt(&self) -> Vec<Value> {
        let task_lines: Vec<String> = self
            .orchestrator_task_list
            .iter()
            .map(|t| {
                let status = match t.status {
                    OrchestratorTaskStatus::InProgress => "in_progress",
                    OrchestratorTaskStatus::Pending => "pending",
                    OrchestratorTaskStatus::Delegated => "delegated",
                    OrchestratorTaskStatus::Completed => "completed",
                    OrchestratorTaskStatus::Failed => "failed",
                    OrchestratorTaskStatus::Cancelled => "cancelled",
                };
                format!("- [{}] {}", status, t.title)
            })
            .collect();

        vec![json!({
            "type": "text",
            "text": format!(
                "SYSTEM MESSAGE:\n\nOrchestrator task list:\n\n{}\n\n\
                 Call orchestrator_task_read if you need to check the current state. \
                 Complete all unfinished tasks. Call orchestrator_task_write to update statuses.",
                task_lines.join("\n"),
            )
        })]
    }
}

// ─── Tests (ported from crow-ade) ─────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(id: &str, title: &str) -> Task {
        Task {
            id: id.to_string(),
            title: title.to_string(),
            description: None,
            status: TaskStatus::Pending,
            priority: "medium".to_string(),
            assigned_to: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn in_progress(id: &str, title: &str) -> Task {
        let mut t = pending(id, title);
        t.status = TaskStatus::InProgress;
        t
    }

    fn completed(id: &str, title: &str) -> Task {
        let mut t = pending(id, title);
        t.status = TaskStatus::Completed;
        t
    }

    fn failed(id: &str, title: &str) -> Task {
        let mut t = pending(id, title);
        t.status = TaskStatus::Failed;
        t
    }

    fn text_of(blocks: &[Value]) -> &str {
        blocks
            .first()
            .and_then(|b| b.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("")
    }

    #[test]
    fn empty_list_stops() {
        let mut s = OrchestrationState::default();
        assert_eq!(s.determine_next_prompt(), None);
    }

    #[test]
    fn all_done_stops() {
        let mut s = OrchestrationState {
            task_list: vec![completed("t1", "first"), completed("t2", "second")],
            ..Default::default()
        };
        assert_eq!(s.determine_next_prompt(), None);
    }

    #[test]
    fn failed_tasks_count_as_done() {
        let mut s = OrchestrationState {
            task_list: vec![failed("t1", "first"), completed("t2", "second")],
            ..Default::default()
        };
        assert_eq!(s.determine_next_prompt(), None);
    }

    #[test]
    fn pending_tasks_prompted_without_promotion() {
        let mut s = OrchestrationState {
            task_list: vec![pending("t1", "first"), pending("t2", "second")],
            ..Default::default()
        };

        let blocks = s.determine_next_prompt().expect("should prompt");
        assert!(text_of(&blocks).contains("first"));
        assert!(text_of(&blocks).contains("second"));
        assert!(text_of(&blocks).contains("task_read"));
        assert!(text_of(&blocks).contains("task_write"));

        assert_eq!(s.task_list[0].status, TaskStatus::Pending);
        assert_eq!(s.task_list[1].status, TaskStatus::Pending);
    }

    #[test]
    fn in_progress_task_keeps_prompting() {
        let mut s = OrchestrationState {
            task_list: vec![in_progress("t1", "first")],
            ..Default::default()
        };

        let blocks = s.determine_next_prompt().expect("should prompt");
        assert!(text_of(&blocks).contains("first"));
    }

    #[test]
    fn mixed_pending_and_completed_prompts() {
        let mut s = OrchestrationState {
            task_list: vec![completed("t1", "first"), pending("t2", "second")],
            ..Default::default()
        };

        let blocks = s.determine_next_prompt().expect("should prompt");
        let text = text_of(&blocks);
        assert!(text.contains("second"));
        assert!(text.contains("first"));
    }

    #[test]
    fn set_caller_stores_session_id() {
        let mut s = OrchestrationState::default();
        assert!(s.caller_session_id.is_none());
        s.set_caller("orchestrator-sid".to_string());
        assert_eq!(s.caller_session_id.as_deref(), Some("orchestrator-sid"));
    }

    #[test]
    fn full_loop_two_tasks_then_stop() {
        let mut s = OrchestrationState {
            task_list: vec![pending("t1", "first"), pending("t2", "second")],
            ..Default::default()
        };

        let b = s.determine_next_prompt().expect("should prompt");
        assert!(text_of(&b).contains("first"));
        assert_eq!(s.task_list[0].status, TaskStatus::Pending);

        s.task_list = vec![completed("t1-new", "first"), pending("t2", "second")];

        let b = s.determine_next_prompt().expect("should prompt");
        assert!(text_of(&b).contains("second"));
        assert_eq!(s.task_list[1].status, TaskStatus::Pending);

        s.task_list = vec![completed("t2-new", "second")];

        assert_eq!(s.determine_next_prompt(), None);
    }

    // ─── Orchestrator task helpers ──────────────────────────────────────────

    fn orch_pending(id: &str, title: &str) -> OrchestratorTask {
        OrchestratorTask {
            id: id.to_string(),
            title: title.to_string(),
            description: None,
            status: OrchestratorTaskStatus::Pending,
            priority: "medium".to_string(),
            assigned_to: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn orch_in_progress(id: &str, title: &str) -> OrchestratorTask {
        let mut t = orch_pending(id, title);
        t.status = OrchestratorTaskStatus::InProgress;
        t
    }

    fn orch_delegated(id: &str, title: &str) -> OrchestratorTask {
        let mut t = orch_pending(id, title);
        t.status = OrchestratorTaskStatus::Delegated;
        t
    }

    fn orch_completed(id: &str, title: &str) -> OrchestratorTask {
        let mut t = orch_pending(id, title);
        t.status = OrchestratorTaskStatus::Completed;
        t
    }

    #[test]
    fn orchestrator_empty_list_stops() {
        let mut s = OrchestrationState::default();
        assert_eq!(s.determine_next_orchestrator_prompt(), None);
    }

    #[test]
    fn orchestrator_pending_prompts() {
        let mut s = OrchestrationState {
            orchestrator_task_list: vec![orch_pending("t1", "first")],
            ..Default::default()
        };
        let blocks = s
            .determine_next_orchestrator_prompt()
            .expect("should prompt");
        let text = text_of(&blocks);
        assert!(text.contains("first"));
        assert!(text.contains("orchestrator_task_read"));
        assert!(text.contains("orchestrator_task_write"));
    }

    #[test]
    fn orchestrator_delegated_pauses_loop() {
        let mut s = OrchestrationState {
            orchestrator_task_list: vec![orch_delegated("t1", "first")],
            ..Default::default()
        };
        assert_eq!(s.determine_next_orchestrator_prompt(), None);
    }

    #[test]
    fn orchestrator_mixed_delegated_then_pending_prompts_first() {
        let mut s = OrchestrationState {
            orchestrator_task_list: vec![
                orch_completed("t1", "first"),
                orch_delegated("t2", "second"),
                orch_pending("t3", "third"),
            ],
            ..Default::default()
        };
        assert_eq!(s.determine_next_orchestrator_prompt(), None);
    }

    #[test]
    fn orchestrator_in_progress_prompts() {
        let mut s = OrchestrationState {
            orchestrator_task_list: vec![
                orch_completed("t1", "first"),
                orch_in_progress("t2", "second"),
            ],
            ..Default::default()
        };
        let blocks = s
            .determine_next_orchestrator_prompt()
            .expect("should prompt");
        let text = text_of(&blocks);
        assert!(text.contains("second"));
    }

    #[test]
    fn orchestrator_all_done_stops() {
        let mut s = OrchestrationState {
            orchestrator_task_list: vec![
                orch_completed("t1", "first"),
                orch_completed("t2", "second"),
            ],
            ..Default::default()
        };
        assert_eq!(s.determine_next_orchestrator_prompt(), None);
    }
}
