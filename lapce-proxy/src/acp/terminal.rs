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

use alacritty_terminal::{
    Term,
    event::EventListener,
    grid::Dimensions,
    index::{Column, Point},
    term::{Config, test::TermSize},
    vte::ansi,
};

/// PTY grid dimensions. MUST match the `WindowSize` handed to the PTY in
/// `pty.rs` (24×80) so the grid parses output exactly as the child emitted it.
const GRID_ROWS: usize = 24;
const GRID_COLS: usize = 80;

/// No-op event listener: the proxy grid is read-only state, never driven by a UI.
struct NoopListener;
impl EventListener for NoopListener {
    fn send_event(&self, _event: alacritty_terminal::event::Event) {}
}

/// Plain-text snapshot of a parsed grid — a port of zed's `content_text`
/// (`crates/terminal/src/alacritty.rs`). Reads every cell from `topmost_line`
/// (oldest history) to `bottommost_line` (last screen row); ANSI escapes were
/// interpreted into cell colors by the parser, so the result is clean text with
/// no escape codes — exactly what the model should see. Trailing blank lines
/// (alacritty pads the unused screen rows with empty lines) are trimmed so the
/// agent isn't handed a wall of newlines.
fn content_text(term: &Term<NoopListener>) -> String {
    let start = Point::new(term.topmost_line(), Column(0));
    let end = Point::new(term.bottommost_line(), term.last_column());
    let text = term.bounds_to_string(start, end);
    text.trim_end_matches(|c: char| matches!(c, '\n' | '\r' | ' ' | '\t'))
        .to_string()
}

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
///
/// Output is parsed into an alacritty grid (fed by the PTY drain thread) rather
/// than accumulated as raw bytes — mirroring zed's ACP client. Reading it back
/// for the agent (`output_string`) yields plain text: ANSI escapes were
/// interpreted into cell colors, so they never reach the model, while the chat
/// UI still streams the raw PTY bytes live (via `on_data`) for a colorful human
/// terminal. The two consumers never share a buffer.
pub struct AcpTerminal {
    /// The parsed grid + its ANSI parser. The drain thread writes (via
    /// `append`), the dispatch thread reads (via `output_string`).
    grid: Mutex<(ansi::Processor, Term<NoopListener>)>,
    /// Current exit state.
    exit: Mutex<Exit>,
    /// Wakes `wait_exit` waiters when the process exits.
    exit_cvar: Condvar,
}

impl AcpTerminal {
    /// Create a new terminal in the running state, behind an `Arc` so the
    /// drain thread and the dispatch thread can share it.
    pub fn new() -> Arc<Self> {
        let size = TermSize::new(GRID_COLS, GRID_ROWS);
        let term = Term::new(Config::default(), &size, NoopListener);
        Arc::new(Self {
            grid: Mutex::new((ansi::Processor::new(), term)),
            exit: Mutex::new(Exit::Running),
            exit_cvar: Condvar::new(),
        })
    }

    /// Feed raw output bytes through the ANSI parser into the grid. Called from
    /// the drain thread for every chunk read from the process. Empty slices are
    /// a no-op.
    pub fn append(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut grid = self.grid.lock().unwrap();
        let (parser, term) = &mut *grid;
        for &byte in bytes {
            parser.advance(term, byte);
        }
    }

    /// Mark the process exited and wake any `wait_exit` waiters. `code` is
    /// `None` if the process was terminated by a signal (no exit code).
    pub fn set_exit(&self, code: Option<i32>) {
        let mut exit = self.exit.lock().unwrap();
        *exit = Exit::Exited(code);
        self.exit_cvar.notify_all();
    }

    /// The terminal's content as plain text for the agent (zed's
    /// `content_text`). ANSI escapes were interpreted into cell colors by the
    /// parser, so the result carries no escape codes — the model sees clean
    /// text while the human UI keeps its colors.
    pub fn output_string(&self) -> String {
        let grid = self.grid.lock().unwrap();
        content_text(&grid.1)
    }

    /// The length (in bytes) of the plain-text content. Used by tests.
    pub fn output_len(&self) -> usize {
        self.output_string().len()
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
        // The grid normalises CRLF to LF when read back as text.
        assert_eq!(term.output_string(), "line1\nline2\nline3");
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
    fn output_string_survives_invalid_utf8() {
        let term = AcpTerminal::new();
        // 0xFF is never valid UTF-8; feeding arbitrary bytes must not panic and
        // must preserve the surrounding printable characters.
        term.append(&[b'a', 0xFF, b'b']);
        let out = term.output_string();
        assert!(out.starts_with('a'), "got: {out:?}");
        assert!(out.ends_with('b'), "got: {out:?}");
    }

    #[test]
    fn output_string_strips_ansi_colors() {
        // The headline behaviour: SGR colour codes are interpreted into cell
        // attributes by the parser, so the text handed to the agent is clean.
        let term = AcpTerminal::new();
        term.append(b"\x1b[31mRED\x1b[0m \x1b[1;32mgreen-bold\x1b[0m normal\r\n");
        let out = term.output_string();
        assert_eq!(out, "RED green-bold normal");
        assert!(!out.contains('\x1b'), "no escape codes may reach the agent: {out:?}");
        assert!(!out.contains('['), "no CSI residue may reach the agent: {out:?}");
    }

    #[test]
    fn output_string_normalizes_carriage_return_overwrite() {
        // A raw byte-strip would leave "1%\r100%\rdone"; the grid yields the
        // final rendered line, exactly what a human reads (zed's advantage over
        // stripping). Each rewrite is the same width so it fully replaces.
        let term = AcpTerminal::new();
        term.append(b"1%\r100%\rdone\r\n");
        let out = term.output_string();
        assert_eq!(out, "done");
        assert!(!out.contains("1%"), "stale frame must be gone: {out:?}");
    }

    #[test]
    fn output_string_strips_cursor_movement_and_clears() {
        let term = AcpTerminal::new();
        // Cursor up / erase-in-line / clear-screen sequences leave no trace.
        term.append(b"noise\x1b[2J\x1b[Hresult\x1b[K\r\n");
        let out = term.output_string();
        assert!(!out.contains('\x1b'), "got: {out:?}");
        assert!(out.contains("result"), "got: {out:?}");
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
        assert_eq!(term.output_string(), "done");
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
        // Reader polls the plain-text snapshot while the writer runs — must
        // never panic (the grid mutex serialises access).
        let reader = term.clone();
        let r = thread::spawn(move || {
            while !reader.exited() {
                let _ = reader.output_string();
                thread::sleep(Duration::from_millis(1));
            }
            reader.output_string()
        });
        w.join().unwrap();
        let out = r.join().unwrap();
        // 1000 lines fit in the default scrollback, so the head and tail both
        // survive; CRLF reads back as LF.
        assert!(out.starts_with("line 0\n"), "got head: {:?}", &out[..20.min(out.len())]);
        assert!(out.contains("line 999"), "tail must be present");
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
