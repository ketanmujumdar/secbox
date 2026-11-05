//! Runs a program with a fixed argument list and no shell.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use zeroize::Zeroizing;

use crate::error::{EngineError, Result};
use crate::redact::Redactor;

/// The most output kept from one stream. The rest is read and discarded so that
/// the child cannot block on a full pipe.
const MAX_CAPTURE_BYTES: u64 = 1024 * 1024;

pub(crate) struct Finished {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

/// "Text file busy": the program is open for writing somewhere.
const EXECUTABLE_BUSY: i32 = 26;
const BUSY_ATTEMPTS: u32 = 40;
const BUSY_GAP: Duration = Duration::from_millis(5);

/// Starts the program, waiting briefly if it is still being written.
///
/// A program that has just been written cannot be run while any handle to it
/// is still open for writing, and in a process with several threads one
/// thread's fork can hold such a handle open across another thread's exec.
/// That window is short, so a few attempts close it. It matters in the same
/// way for an engine binary that was just downloaded or replaced.
fn retry_while_busy<T>(
    mut spawn: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    for _ in 1..BUSY_ATTEMPTS {
        match spawn() {
            Err(e) if e.raw_os_error() == Some(EXECUTABLE_BUSY) => thread::sleep(BUSY_GAP),
            other => return other,
        }
    }
    spawn()
}

fn drain(mut stream: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut kept = Vec::new();
        let _ = stream
            .by_ref()
            .take(MAX_CAPTURE_BYTES)
            .read_to_end(&mut kept);
        let _ = std::io::copy(&mut stream, &mut std::io::sink());
        kept
    })
}

/// Runs with a cleared environment plus `env`, and null stdin. Both output
/// streams are redacted before they leave this function.
pub(crate) fn run(
    program: &Path,
    args: &[String],
    env: &BTreeMap<String, String>,
    timeout: Duration,
    operation: &str,
    redactor: &dyn Redactor,
) -> Result<Finished> {
    run_with_input(program, args, env, timeout, operation, redactor, None)
}

/// The same, with bytes written to the child's standard input.
///
/// This is how a secret reaches the guest. Standard input is not in the
/// argument list, so it cannot be read from the host's process list, and it
/// is never written to a file on the way. The buffer is zeroed after it is
/// written.
pub(crate) fn run_with_input(
    program: &Path,
    args: &[String],
    env: &BTreeMap<String, String>,
    timeout: Duration,
    operation: &str,
    redactor: &dyn Redactor,
    input: Option<Zeroizing<Vec<u8>>>,
) -> Result<Finished> {
    let spawn = || {
        Command::new(program)
            .args(args)
            .env_clear()
            .envs(env)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    };
    let mut child = retry_while_busy(spawn).map_err(|e| EngineError::Spawn {
        operation: operation.to_owned(),
        reason: redactor.redact(&e.to_string()),
    })?;

    if let Some(bytes) = input {
        match child.stdin.take() {
            // Written on its own thread, so a child that writes a great deal
            // before reading cannot deadlock against a full pipe.
            Some(mut pipe) => {
                thread::spawn(move || {
                    let _ = pipe.write_all(&bytes);
                    let _ = pipe.flush();
                    drop(pipe);
                    drop(bytes);
                });
            }
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(EngineError::Spawn {
                    operation: operation.to_owned(),
                    reason: "the input pipe was not created".to_owned(),
                });
            }
        }
    }

    let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(EngineError::Spawn {
            operation: operation.to_owned(),
            reason: "output pipes were not created".to_owned(),
        });
    };
    let out = drain(out);
    let err = drain(err);

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                // The reader threads are left to finish on their own: a grandchild
                // holding a pipe open must not block the caller past the limit.
                return Err(EngineError::Timeout {
                    operation: operation.to_owned(),
                    seconds: timeout.as_secs(),
                });
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(EngineError::Spawn {
                    operation: operation.to_owned(),
                    reason: redactor.redact(&e.to_string()),
                });
            }
        }
    };

    let text = |handle: thread::JoinHandle<Vec<u8>>| {
        let bytes = handle.join().unwrap_or_default();
        redactor.redact(&String::from_utf8_lossy(&bytes))
    };
    Ok(Finished {
        status,
        stdout: text(out),
        stderr: text(err),
    })
}
