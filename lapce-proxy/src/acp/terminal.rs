//! Client-side ACP terminal state.
//!
//! When an ACP agent (e.g. crow-cli) uses the **client** terminal tools, the
//! editor — not the agent — spawns the real process. lapce must therefore:
//!
//!   * stream the process output to the chat UI live (the `terminal/create`
//!     drain thread sends `AcpTerminalData` to the app), AND
//!   * accumulate that same output so `terminal/output` can hand it back to the
//!     agent (this is what the model actually "sees"), AND
//!   * track the exit code so `terminal/waitForExit` can block until the
//!     command finishes and `terminal/output` can report `exitStatus`.
//!
//! `AcpTerminal` is the shared state between the drain thread (writer) and the
//! dispatch thread (reader for `terminal/output` / `terminal/waitForExit`). It
//! is deliberately pure — no RPC, no IO, no process — so it can be unit-tested
//! headlessly. The wire shapes it feeds are verified against crow-ade's
//! `crow-acp` serialization tests and the `agent-client-protocol-schema` crate:
//!
//!   * `terminal/output`    → `{ output, truncated, exitStatus?: { exitCode } }`
//!   * `terminal/waitForExit` → `{ exitCode, signal }` (TerminalExitStatus, flattened)
//!   * `terminal/create`    → `{ terminalId }`

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Exit state of the terminal's process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Exit {
    /// Still running.
    Running,
    /// Exited. Inner `None` means "terminated by signal / no exit code".
    Exited(Option<i32>),
}

impl Default for Exit {
    fn default() -> Self {
        Exit::Running
    }
}

/// Shared state for one client-side ACP terminal.
#[derive(Default)]
pub struct AcpTerminal {
    /// Raw output bytes accumulated from the process (stdout + stderr).
    output: Mutex<Vec<u8>>,
    /// Current exit state.
    exit: Mutex<Exit>,
    /// Wakes `wait_exit` waiters when the process exits.
    exit_cvar: Condvar,
}

impl AcpTerminal {
    /// Create a new terminal in the running state, behind an `Arc` so the
    /// drain thread and the dispatch thread can share it.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Append raw output bytes. Called from the drain thread for every chunk
    /// read from the process. Empty slices are a no-op.
    pub fn append(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.output.lock().unwrap().extend_from_slice(bytes);
    }

    /// Mark the process exited and wake any `wait_exit` waiters. `code` is
    /// `None` if the process was terminated by a signal (no exit code).
    pub fn set_exit(&self, code: Option<i32>) {
        let mut exit = self.exit.lock().unwrap();
        *exit = Exit::Exited(code);
        self.exit_cvar.notify_all();
    }

    /// The accumulated output as a (lossy) UTF-8 string. Invalid byte
    /// sequences are replaced with U+FFFD rather than panicking — terminal
    /// output is arbitrary bytes.
    pub fn output_string(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
    }

    /// The number of output bytes accumulated so far.
    pub fn output_len(&self) -> usize {
        self.output.lock().unwrap().len()
    }

    /// The exit code, if the process exited with one. Returns `None` both
    /// while still running and when terminated by signal — pair with
    /// [`Self::exited`] to disambiguate.
    pub fn exit_code(&self) -> Option<i32> {
        match *self.exit.lock().unwrap() {
            Exit::Exited(code) => code,
            Exit::Running => None,
        }
    }

    /// Whether the process has exited (regardless of code/signal).
    pub fn exited(&self) -> bool {
        matches!(*self.exit.lock().unwrap(), Exit::Exited(_))
    }

    /// Block until the process exits, then return its exit code (or `None` if
    /// it was terminated by signal). With `timeout`, gives up after the
    /// duration and returns `None` if still running.
    ///
    /// This is what backs `terminal/waitForExit`: the agent awaits it so that
    /// the subsequent `terminal/output` sees the complete output.
    pub fn wait_exit(&self, timeout: Option<Duration>) -> Option<i32> {
        let mut exit = self.exit.lock().unwrap();
        match timeout {
            None => {
                while matches!(*exit, Exit::Running) {
                    exit = self.exit_cvar.wait(exit).unwrap();
                }
            }
            Some(t) => {
                let (guard, _) = self
                    .exit_cvar
                    .wait_timeout_while(exit, t, |e| matches!(*e, Exit::Running))
                    .unwrap();
                exit = guard;
            }
        }
        match *exit {
            Exit::Exited(code) => code,
            Exit::Running => None,
        }
    }
}

/// Keep only the last `limit` bytes of `output` (the tail), starting on a
/// UTF-8 char boundary and at a line start. Mirrors crow-ade's
/// `truncate_tail`: for a long-running command the tail (final results, exit
/// status, errors) is what an agent usually needs. `limit = None` returns the
/// output unchanged. Returns `(text, truncated)`.
pub fn truncate_tail(output: &str, limit: Option<usize>) -> (String, bool) {
    let Some(limit) = limit else {
        return (output.to_string(), false);
    };
    if output.len() <= limit {
        return (output.to_string(), false);
    }
    // Keep the tail. Start at `len - limit` and advance to the next char
    // boundary so we never begin mid-codepoint.
    let mut start = output.len().saturating_sub(limit);
    while !output.is_char_boundary(start) {
        start += 1;
    }
    // Drop a partial leading line so the retained tail begins at a line start.
    if let Some(nl) = output[start..].find('\n') {
        start += nl + 1;
    }
    (output[start..].to_string(), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn append_and_output_string() {
        let term = AcpTerminal::new();
        term.append(b"hello world");
        assert_eq!(term.output_string(), "hello world");
        assert_eq!(term.output_len(), 11);
    }

    #[test]
    fn append_multiple_accumulates_in_order() {
        let term = AcpTerminal::new();
        term.append(b"line1\r\n");
        term.append(b"line2\r\n");
        term.append(b"line3");
        assert_eq!(term.output_string(), "line1\r\nline2\r\nline3");
    }

    #[test]
    fn append_empty_is_noop() {
        let term = AcpTerminal::new();
        term.append(b"abc");
        term.append(b"");
        assert_eq!(term.output_string(), "abc");
        assert_eq!(term.output_len(), 3);
    }

    #[test]
    fn output_string_is_lossy_for_invalid_utf8() {
        let term = AcpTerminal::new();
        // 0xFF is never valid UTF-8; must not panic, replaced with U+FFFD.
        term.append(&[b'a', 0xFF, b'b']);
        let out = term.output_string();
        assert!(out.starts_with('a'));
        assert!(out.ends_with('b'));
        assert!(out.contains('\u{FFFD}'));
    }

    #[test]
    fn exit_code_none_and_not_exited_while_running() {
        let term = AcpTerminal::new();
        assert!(!term.exited());
        assert_eq!(term.exit_code(), None);
    }

    #[test]
    fn set_exit_records_code() {
        let term = AcpTerminal::new();
        term.set_exit(Some(0));
        assert!(term.exited());
        assert_eq!(term.exit_code(), Some(0));
    }

    #[test]
    fn set_exit_nonzero_code() {
        let term = AcpTerminal::new();
        term.set_exit(Some(127));
        assert!(term.exited());
        assert_eq!(term.exit_code(), Some(127));
    }

    #[test]
    fn set_exit_signal_leaves_code_none_but_exited() {
        let term = AcpTerminal::new();
        term.set_exit(None);
        assert!(term.exited(), "terminated-by-signal is still 'exited'");
        assert_eq!(term.exit_code(), None);
    }

    #[test]
    fn wait_exit_returns_code_set_by_another_thread() {
        let term = AcpTerminal::new();
        let writer = term.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            writer.append(b"done\r\n");
            writer.set_exit(Some(0));
        });
        let code = term.wait_exit(None);
        assert_eq!(code, Some(0));
        assert_eq!(term.output_string(), "done\r\n");
    }

    #[test]
    fn wait_exit_returns_immediately_if_already_exited() {
        let term = AcpTerminal::new();
        term.set_exit(Some(3));
        let start = Instant::now();
        let code = term.wait_exit(Some(Duration::from_secs(5)));
        assert_eq!(code, Some(3));
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "should not wait when already exited"
        );
    }

    #[test]
    fn wait_exit_timeout_returns_none_while_running() {
        let term = AcpTerminal::new();
        let start = Instant::now();
        let code = term.wait_exit(Some(Duration::from_millis(80)));
        assert_eq!(code, None, "still running → None after timeout");
        assert!(start.elapsed() >= Duration::from_millis(70));
    }

    #[test]
    fn wait_exit_wakes_all_waiters() {
        let term = AcpTerminal::new();
        let mut handles = Vec::new();
        for _ in 0..4 {
            let t = term.clone();
            handles.push(thread::spawn(move || t.wait_exit(None)));
        }
        thread::sleep(Duration::from_millis(30));
        term.set_exit(Some(0));
        for h in handles {
            assert_eq!(h.join().unwrap(), Some(0));
        }
    }

    #[test]
    fn concurrent_append_and_read_is_safe() {
        let term = AcpTerminal::new();
        let writer = term.clone();
        let w = thread::spawn(move || {
            for i in 0..1000 {
                writer.append(format!("line {i}\r\n").as_bytes());
            }
            writer.set_exit(Some(0));
        });
        // Reader polls output while the writer runs — must never panic.
        let reader = term.clone();
        let r = thread::spawn(move || {
            let mut last = 0;
            while !reader.exited() {
                let len = reader.output_len();
                assert!(len >= last, "output must only grow");
                last = len;
                thread::sleep(Duration::from_millis(1));
            }
            reader.output_string()
        });
        w.join().unwrap();
        let out = r.join().unwrap();
        assert!(out.contains("line 999"));
        assert!(out.starts_with("line 0\r\n"));
    }

    // ---- truncate_tail ----

    #[test]
    fn truncate_tail_none_limit_returns_unchanged() {
        let (text, truncated) = truncate_tail("hello\nworld", None);
        assert_eq!(text, "hello\nworld");
        assert!(!truncated);
    }

    #[test]
    fn truncate_tail_under_limit_unchanged() {
        let (text, truncated) = truncate_tail("short", Some(100));
        assert_eq!(text, "short");
        assert!(!truncated);
    }

    #[test]
    fn truncate_tail_equal_limit_unchanged() {
        let s = "abcde";
        let (text, truncated) = truncate_tail(s, Some(5));
        assert_eq!(text, "abcde");
        assert!(!truncated);
    }

    #[test]
    fn truncate_tail_over_limit_keeps_tail_on_line_start() {
        let output = "line1\nline2\nline3\nline4\n";
        // Limit small enough to drop the head; tail should begin at a line
        // start and include the final lines.
        let (text, truncated) = truncate_tail(output, Some(12));
        assert!(truncated);
        assert!(text.contains("line4"));
        assert!(!text.contains("line1"), "head should be dropped: {text:?}");
        // Must start at a line boundary (no partial line).
        assert!(
            text.starts_with("line"),
            "tail should start at a line start: {text:?}"
        );
    }

    #[test]
    fn truncate_tail_respects_char_boundary() {
        // Each '€' is 3 bytes. Build a string and cut with a limit that would
        // land mid-codepoint; the result must still be valid UTF-8.
        let output = "€€€€€€€€€€"; // 30 bytes
        let (text, truncated) = truncate_tail(output, Some(7));
        assert!(truncated);
        // Validity: round-trips through UTF-8 without replacement chars.
        assert!(!text.contains('\u{FFFD}'), "must not split a codepoint: {text:?}");
        assert!(text.chars().all(|c| c == '€'));
    }

    #[test]
    fn truncate_tail_empty_output() {
        let (text, truncated) = truncate_tail("", Some(10));
        assert_eq!(text, "");
        assert!(!truncated);
    }
}
