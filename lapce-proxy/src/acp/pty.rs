//! Real PTY backing for ACP client-side terminals.
//!
//! crow-cli's `execute` tool sends `terminal/create` with a single `command`
//! string (e.g. `"ls -la"`). Like crow-ade's `crow-acp`, we run it in a **real
//! PTY** via `shell -c "<command>"` (so shell features and isatty behaviour
//! work), not a plain piped subprocess. The PTY is created with lapce's own
//! `alacritty_terminal::tty` (the same backend the terminal panel uses) and
//! driven by a poller loop mirroring `lapce-proxy/src/terminal.rs`.
//!
//! `run_acp_pty` is decoupled from RPC via an `on_data` callback so it can be
//! unit-tested headlessly by spawning a real PTY and capturing its output —
//! the same approach as crow-ade's `pty_read_output_*` tests.

use std::collections::HashMap;
use std::io::{ErrorKind, Read};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use alacritty_terminal::{
    event::WindowSize,
    tty::{self, EventedPty, EventedReadWrite, Options, Shell, setup_env},
};
use polling::PollMode;

use super::AcpTerminal;

/// Poller token for the PTY master read/write fd. alacritty registers the
/// child-event fd at `token + 1`, so this must be 0 and the child token 1
/// (same convention as `terminal.rs`).
const PTY_READ_WRITE_TOKEN: usize = 0;
const PTY_CHILD_EVENT_TOKEN: usize = 1;

/// Pick the user's shell for `shell -c "<command>"`. Mirrors crow-ade's
/// `detect_default_shell`: honour `$SHELL`, fall back to a sane default.
pub fn detect_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// Spawn a real PTY running `shell -c "<command>"`.
pub fn spawn_acp_pty(
    command: &str,
    cwd: Option<PathBuf>,
    env: HashMap<String, String>,
) -> std::io::Result<tty::Pty> {
    let shell = Shell::new(
        detect_shell(),
        vec!["-c".to_string(), command.to_string()],
    );
    let options = Options {
        shell: Some(shell),
        working_directory: cwd,
        hold: false,
        env,
    };
    setup_env();
    let size = WindowSize {
        num_lines: 24,
        num_cols: 80,
        cell_width: 1,
        cell_height: 1,
    };
    tty::new(&options, size, 0)
}

/// Drive the PTY until the child exits. Every chunk read is appended to
/// `term` (so `terminal/output` can return it to the agent) AND passed to
/// `on_data` (so the UI can stream it live). Returns the exit code (or `None`
/// if terminated by signal / unreadable). Sets `term`'s exit state before
/// returning so `wait_exit` waiters wake promptly.
pub fn run_acp_pty(
    mut pty: tty::Pty,
    term: Arc<AcpTerminal>,
    mut on_data: impl FnMut(&[u8]),
) -> Option<i32> {
    let poller: Arc<polling::Poller> = match polling::Poller::new() {
        Ok(p) => p.into(),
        Err(err) => {
            tracing::error!("ACP pty: poller creation failed: {err}");
            term.set_exit(None);
            return None;
        }
    };

    let mut buf = [0u8; 0x10_0000];
    let poll_opts = PollMode::Level;
    let interest = polling::Event::readable(PTY_READ_WRITE_TOKEN);

    // Register the PTY (master fd at PTY_READ_WRITE_TOKEN, child-event fd at
    // PTY_CHILD_EVENT_TOKEN).
    unsafe {
        if let Err(err) = pty.register(&poller, interest, poll_opts) {
            tracing::error!("ACP pty: register failed: {err}");
            term.set_exit(None);
            return None;
        }
    }

    let mut events =
        polling::Events::with_capacity(NonZeroUsize::new(1024).unwrap());
    let timeout = Some(Duration::from_secs(6));
    let mut exit_code: Option<i32> = None;

    'event_loop: loop {
        events.clear();
        match poller.wait(&mut events, timeout) {
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => {
                tracing::error!("ACP pty: poll error: {err}");
                break;
            }
        }

        for event in events.iter() {
            match event.key {
                PTY_CHILD_EVENT_TOKEN => {
                    if let Some(tty::ChildEvent::Exited(code)) =
                        pty.next_child_event()
                    {
                        // Final drain: capture the last burst the reader may
                        // have pushed after the child was reaped (common with
                        // pipes like `find | head`).
                        let _ = read_pty(&mut pty, &term, &mut on_data, &mut buf);
                        exit_code = code;
                        break 'event_loop;
                    }
                }
                PTY_READ_WRITE_TOKEN => {
                    if event.is_interrupt() {
                        continue;
                    }
                    if event.readable {
                        if let Err(err) =
                            read_pty(&mut pty, &term, &mut on_data, &mut buf)
                        {
                            // On Linux a read on the master side can fail with
                            // EIO when the client hangs up; loop back for the
                            // inevitable Exited event (same as terminal.rs).
                            #[cfg(target_os = "linux")]
                            if err.raw_os_error() == Some(libc::EIO) {
                                continue;
                            }
                            tracing::error!("ACP pty: read error: {err}");
                            break 'event_loop;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    if let Err(err) = pty.deregister(&poller) {
        tracing::error!("ACP pty: deregister failed: {err}");
    }
    term.set_exit(exit_code);
    exit_code
}

/// Read available PTY output until it would block, appending to `term` and
/// forwarding each chunk to `on_data`. Mirrors `terminal.rs::pty_read`.
fn read_pty(
    pty: &mut tty::Pty,
    term: &AcpTerminal,
    on_data: &mut impl FnMut(&[u8]),
    buf: &mut [u8],
) -> std::io::Result<()> {
    loop {
        match pty.reader().read(buf) {
            Ok(0) => break,
            Ok(n) => {
                term.append(&buf[..n]);
                on_data(&buf[..n]);
            }
            Err(err) => match err.kind() {
                ErrorKind::Interrupted | ErrorKind::WouldBlock => break,
                _ => return Err(err),
            },
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Run a command in a real PTY and assert the output is captured into the
    /// AcpTerminal AND streamed via on_data, with exit code 0.
    #[test]
    fn pty_runs_command_and_captures_output() {
        let pty =
            spawn_acp_pty("echo hello_acp_pty", None, HashMap::new())
                .expect("spawn pty");
        let term = AcpTerminal::new();
        let streamed = Arc::new(Mutex::new(Vec::<u8>::new()));
        let streamed2 = streamed.clone();
        let code = run_acp_pty(pty, term.clone(), move |bytes| {
            streamed2.lock().unwrap().extend_from_slice(bytes);
        });
        assert_eq!(code, Some(0), "echo should exit 0");
        assert!(term.exited());
        assert_eq!(term.exit_code(), Some(0));
        let out = term.output_string();
        assert!(out.contains("hello_acp_pty"), "got: {out:?}");
        // on_data saw the same bytes the terminal accumulated.
        let s = String::from_utf8_lossy(&streamed.lock().unwrap()).into_owned();
        assert!(s.contains("hello_acp_pty"), "streamed: {s:?}");
    }

    /// A non-zero exit code is reported.
    #[test]
    fn pty_reports_nonzero_exit_code() {
        let pty = spawn_acp_pty("exit 7", None, HashMap::new())
            .expect("spawn pty");
        let term = AcpTerminal::new();
        let code = run_acp_pty(pty, term.clone(), |_| {});
        assert_eq!(code, Some(7));
        assert_eq!(term.exit_code(), Some(7));
        assert!(term.exited());
    }

    /// Multi-line output is fully captured (not just the first chunk).
    #[test]
    fn pty_captures_multiline_output() {
        let pty = spawn_acp_pty(
            "for i in 1 2 3; do echo line_$i; done",
            None,
            HashMap::new(),
        )
        .expect("spawn pty");
        let term = AcpTerminal::new();
        let code = run_acp_pty(pty, term.clone(), |_| {});
        assert_eq!(code, Some(0));
        let out = term.output_string();
        assert!(out.contains("line_1"), "got: {out:?}");
        assert!(out.contains("line_2"), "got: {out:?}");
        assert!(out.contains("line_3"), "got: {out:?}");
    }

    /// The cwd is honoured by the spawned shell.
    #[test]
    fn pty_respects_cwd() {
        let pty =
            spawn_acp_pty("pwd", Some(PathBuf::from("/tmp")), HashMap::new())
                .expect("spawn pty");
        let term = AcpTerminal::new();
        let code = run_acp_pty(pty, term.clone(), |_| {});
        assert_eq!(code, Some(0));
        let out = term.output_string();
        // /tmp resolves to /private/tmp on macOS; both contain "tmp".
        assert!(out.contains("tmp"), "got: {out:?}");
    }

    /// Environment variables passed to terminal/create reach the command.
    #[test]
    fn pty_passes_env_vars() {
        let mut env = HashMap::new();
        env.insert("ACP_TEST_VAR".to_string(), "acp_env_value".to_string());
        let pty = spawn_acp_pty("echo $ACP_TEST_VAR", None, env)
            .expect("spawn pty");
        let term = AcpTerminal::new();
        let code = run_acp_pty(pty, term.clone(), |_| {});
        assert_eq!(code, Some(0));
        let out = term.output_string();
        assert!(out.contains("acp_env_value"), "got: {out:?}");
    }

    /// A larger burst of output (exercises chunked reads) is captured whole.
    #[test]
    fn pty_captures_large_output() {
        let pty = spawn_acp_pty(
            "for i in $(seq 1 500); do echo row_$i; done",
            None,
            HashMap::new(),
        )
        .expect("spawn pty");
        let term = AcpTerminal::new();
        let code = run_acp_pty(pty, term.clone(), |_| {});
        assert_eq!(code, Some(0));
        let out = term.output_string();
        assert!(out.contains("row_1"), "missing first row");
        assert!(out.contains("row_500"), "missing last row");
    }

    #[test]
    fn detect_shell_is_nonempty() {
        assert!(!detect_shell().is_empty());
    }
}
