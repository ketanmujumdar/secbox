//! Running the engine for one tool call, with its output captured.
//!
//! A terminal session hands the engine the person's own terminal. A tool call
//! cannot: its output has to come back as text, so this captures both streams,
//! with a limit on how much is kept and on how long it may run. A command that
//! writes more than the limit keeps running and its extra output is read and
//! thrown away, so it is never stuck on a full pipe.

use std::io::{self, Read, Write};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How long to wait for the output pipes to close once the engine has exited.
/// A process left behind holding a pipe open must not hold the tool call too.
const DRAIN_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, Default)]
pub(crate) struct Captured {
    /// The exit code, or `None` if it ended on a signal or was stopped.
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// More output came than the limit, and the rest was thrown away.
    pub stdout_cut: bool,
    pub stderr_cut: bool,
    /// The time limit passed, so the engine was stopped.
    pub timed_out: bool,
    /// The output pipes stayed open after the engine ended, so what came
    /// after that was not waited for.
    pub drain_abandoned: bool,
}

#[derive(Default)]
struct Kept {
    bytes: Vec<u8>,
    cut: bool,
}

/// Runs `command` with `input` on its stdin, then closes stdin.
pub(crate) fn run_captured(
    mut command: Command,
    input: Vec<u8>,
    limit: usize,
    timeout: Duration,
) -> io::Result<Captured> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;

    // Written from its own thread, because a command that writes a lot before
    // it reads would otherwise block both sides. A write to a command that has
    // already ended fails, and is not worth reporting.
    if let Some(mut stdin) = child.stdin.take() {
        thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }

    let (done_tx, done_rx) = mpsc::channel();
    let stdout = Arc::new(Mutex::new(Kept::default()));
    let stderr = Arc::new(Mutex::new(Kept::default()));
    let mut readers = 0;
    if let Some(pipe) = child.stdout.take() {
        drain(pipe, Arc::clone(&stdout), limit, done_tx.clone());
        readers += 1;
    }
    if let Some(pipe) = child.stderr.take() {
        drain(pipe, Arc::clone(&stderr), limit, done_tx.clone());
        readers += 1;
    }
    drop(done_tx);

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status: ExitStatus = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            let _ = child.kill();
            break child.wait()?;
        }
        thread::sleep(Duration::from_millis(20));
    };

    let mut drain_abandoned = false;
    let drain_deadline = Instant::now() + DRAIN_WAIT;
    for _ in 0..readers {
        let left = drain_deadline.saturating_duration_since(Instant::now());
        if done_rx.recv_timeout(left).is_err() {
            drain_abandoned = true;
            break;
        }
    }

    let take = |kept: &Arc<Mutex<Kept>>| {
        let mut kept = kept.lock().unwrap_or_else(|e| e.into_inner());
        (std::mem::take(&mut kept.bytes), kept.cut)
    };
    let (stdout, stdout_cut) = take(&stdout);
    let (stderr, stderr_cut) = take(&stderr);
    use std::os::unix::process::ExitStatusExt;
    Ok(Captured {
        code: status.code(),
        signal: status.signal(),
        stdout,
        stderr,
        stdout_cut,
        stderr_cut,
        timed_out,
        drain_abandoned,
    })
}

fn drain(mut pipe: impl Read + Send + 'static, kept: Arc<Mutex<Kept>>, limit: usize, done: mpsc::Sender<()>) {
    thread::spawn(move || {
        let mut buffer = [0u8; 16 * 1024];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let mut kept = kept.lock().unwrap_or_else(|e| e.into_inner());
                    let room = limit.saturating_sub(kept.bytes.len());
                    if read > room {
                        kept.cut = true;
                    }
                    let keep = read.min(room);
                    kept.bytes.extend_from_slice(&buffer[..keep]);
                }
            }
        }
        let _ = done.send(());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }

    #[test]
    fn both_streams_and_the_exit_code_come_back() {
        let done = run_captured(sh("echo out; echo err >&2; exit 3"), Vec::new(), 1024, Duration::from_secs(10)).unwrap();
        assert_eq!(done.code, Some(3));
        assert_eq!(done.stdout, b"out\n");
        assert_eq!(done.stderr, b"err\n");
        assert!(!done.timed_out && !done.stdout_cut && !done.stderr_cut);
    }

    #[test]
    fn input_reaches_stdin_and_then_ends() {
        let done = run_captured(sh("cat"), b"hello".to_vec(), 1024, Duration::from_secs(10)).unwrap();
        assert_eq!(done.code, Some(0));
        assert_eq!(done.stdout, b"hello");
    }

    #[test]
    fn output_over_the_limit_is_cut_and_the_command_still_finishes() {
        let done = run_captured(
            sh("head -c 300000 /dev/zero; echo end >&2"),
            Vec::new(),
            1000,
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(done.code, Some(0));
        assert_eq!(done.stdout.len(), 1000);
        assert!(done.stdout_cut);
        assert_eq!(done.stderr, b"end\n");
    }

    #[test]
    fn a_command_past_its_time_is_stopped() {
        let started = Instant::now();
        let done = run_captured(sh("echo started; exec sleep 30"), Vec::new(), 1024, Duration::from_millis(300)).unwrap();
        assert!(done.timed_out);
        assert_eq!(done.code, None);
        assert_eq!(done.stdout, b"started\n");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_process_left_holding_the_pipe_does_not_hold_the_call() {
        let started = Instant::now();
        let done = run_captured(sh("sleep 30 & echo parent"), Vec::new(), 1024, Duration::from_secs(10)).unwrap();
        assert_eq!(done.code, Some(0));
        assert!(done.drain_abandoned);
        assert!(started.elapsed() < Duration::from_secs(15));
    }
}
