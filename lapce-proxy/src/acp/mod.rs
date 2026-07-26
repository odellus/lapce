//! ACP (Agent Client Protocol) integration for Lapce.
//!
//! Spawns ACP-compatible agent subprocesses (e.g. crow-cli, claude-code)
//! and bridges JSON-RPC over stdio to lapce's proxy ↔ core RPC channel.
//!
//! Ported from crow-ade's `crow-acp` crate, adapted from tokio-async
//! to lapce's synchronous crossbeam-channel architecture.

pub mod agent;
pub mod log;
pub mod orchestration;
pub mod pty;
pub mod session;
pub mod terminal;

pub use agent::AgentConfig;
pub use session::{AcpSession, AcpSessionManager, SessionEvent};
pub use terminal::{truncate_tail, AcpTerminal};
