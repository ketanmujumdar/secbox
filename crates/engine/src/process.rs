//! Runs a program with a fixed argument list and no shell.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| EngineError::Spawn {
            operation: operation.to_owned(),
            reason: redactor.redact(&e.to_string()),
        })?;

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
