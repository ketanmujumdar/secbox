//! Runs a program with a fixed argument list and no shell.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use zeroize::Zeroizing;

use crate::error::{EngineError, Result};
use crate::redact::Redactor;
use crate::types::{OutputSink, OutputStream, StreamEnd};

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

const READ_BYTES: usize = 16 * 1024;
/// How much of a line with no end is held back before it is passed on anyway.
const MAX_HELD_BYTES: usize = 64 * 1024;
/// How often a quiet stream checks whether to stop.
const STREAM_TICK: Duration = Duration::from_millis(100);
/// How long to wait for the rest of the output after the command exits. A
/// grandchild holding a pipe open must not keep the caller here.
const EXIT_GRACE: Duration = Duration::from_millis(500);

fn spawn_reader(
    mut stream: impl Read + Send + 'static,
    which: OutputStream,
    sender: mpsc::SyncSender<(OutputStream, Vec<u8>)>,
) {
    thread::spawn(move || {
        let mut buffer = vec![0u8; READ_BYTES];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(read) => {
                    if sender.send((which, buffer[..read].to_vec())).is_err() {
                        return;
                    }
                }
            }
        }
    });
}

/// Where a run of bytes can be cut without splitting a character.
fn whole_characters(bytes: &[u8]) -> usize {
    let len = bytes.len();
    for back in 1..=len.min(3) {
        let byte = bytes[len - back];
        if byte & 0xc0 == 0x80 {
            continue;
        }
        let width = match byte {
            0xf0..=0xff => 4,
            0xe0..=0xef => 3,
            0xc0..=0xdf => 2,
            _ => 1,
        };
        return if width > back { len - back } else { len };
    }
    len
}

/// Output held back until it can be redacted. A value split across two chunks
/// would otherwise pass through unnoticed, so whole lines are preferred, and a
/// partial line goes out only when the stream goes quiet or it grows too long.
struct Held<'a> {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    redactor: &'a dyn Redactor,
}

impl Held<'_> {
    fn add(&mut self, which: OutputStream, bytes: &[u8], sink: &mut dyn OutputSink) -> bool {
        let held = match which {
            OutputStream::Stdout => &mut self.stdout,
            OutputStream::Stderr => &mut self.stderr,
        };
        held.extend_from_slice(bytes);
        let cut = if held.len() >= MAX_HELD_BYTES {
            whole_characters(held)
        } else {
            held.iter().rposition(|b| *b == b'\n').map_or(0, |at| at + 1)
        };
        self.send(which, cut, sink)
    }

    fn send(&mut self, which: OutputStream, cut: usize, sink: &mut dyn OutputSink) -> bool {
        let held = match which {
            OutputStream::Stdout => &mut self.stdout,
            OutputStream::Stderr => &mut self.stderr,
        };
        if cut == 0 {
            return true;
        }
        let line: Vec<u8> = held.drain(..cut).collect();
        // Invalid bytes become a replacement mark here, which is what the next
        // stage would make of them anyway.
        let text = self.redactor.redact(&String::from_utf8_lossy(&line));
        sink.output(which, text.as_bytes())
    }

    /// Passes on what is held, other than a character still arriving.
    fn quiet(&mut self, sink: &mut dyn OutputSink) -> bool {
        let out = whole_characters(&self.stdout);
        let err = whole_characters(&self.stderr);
        self.send(OutputStream::Stdout, out, sink) && self.send(OutputStream::Stderr, err, sink)
    }

    fn finish(&mut self, sink: &mut dyn OutputSink) -> bool {
        let out = self.stdout.len();
        let err = self.stderr.len();
        self.send(OutputStream::Stdout, out, sink) && self.send(OutputStream::Stderr, err, sink)
    }
}

fn stop(child: &mut Child) -> StreamEnd {
    let _ = child.kill();
    let _ = child.wait();
    StreamEnd::Stopped
}

/// Runs with a cleared environment plus `env` and null stdin, handing output to
/// `sink` as it arrives. Output is redacted before the sink sees it.
///
/// Stopping kills this process only. Measured on v0.7.13, even with SIGKILL the
/// command inside the guest ends when its client goes away, and the sandbox
/// keeps running.
pub(crate) fn stream(
    program: &Path,
    args: &[String],
    env: &BTreeMap<String, String>,
    limit: Option<Duration>,
    operation: &str,
    redactor: &dyn Redactor,
    sink: &mut dyn OutputSink,
) -> Result<(StreamEnd, Option<ExitStatus>)> {
    let spawn_error = |reason: String| EngineError::Spawn {
        operation: operation.to_owned(),
        reason,
    };
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| spawn_error(redactor.redact(&e.to_string())))?;
    let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) else {
        stop(&mut child);
        return Err(spawn_error("output pipes were not created".to_owned()));
    };
    // Small on purpose: a slow reader should slow the command, not fill memory.
    let (sender, receiver) = mpsc::sync_channel(8);
    spawn_reader(out, OutputStream::Stdout, sender.clone());
    spawn_reader(err, OutputStream::Stderr, sender);

    let started = Instant::now();
    let mut held = Held {
        stdout: Vec::new(),
        stderr: Vec::new(),
        redactor,
    };
    let mut exited: Option<(ExitStatus, Instant)> = None;
    loop {
        let open = match receiver.recv_timeout(STREAM_TICK) {
            Ok((which, bytes)) => {
                if !held.add(which, &bytes, sink) {
                    return Ok((stop(&mut child), None));
                }
                true
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if !held.quiet(sink) {
                    return Ok((stop(&mut child), None));
                }
                true
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => false,
        };
        if sink.cancelled() {
            return Ok((stop(&mut child), None));
        }
        if limit.is_some_and(|limit| started.elapsed() >= limit) {
            stop(&mut child);
            return Err(EngineError::Timeout {
                operation: operation.to_owned(),
                seconds: limit.map_or(0, |limit| limit.as_secs()),
            });
        }
        match exited {
            Some((_, at)) if at.elapsed() >= EXIT_GRACE => break,
            Some(_) => {}
            None => match child.try_wait() {
                Ok(Some(status)) => exited = Some((status, Instant::now())),
                Ok(None) => {
                    // Both pipes closed and the command is still going. There
                    // is nothing left to read, so this only waits for its exit.
                    if !open {
                        thread::sleep(Duration::from_millis(10));
                    }
                }
                Err(e) => {
                    stop(&mut child);
                    return Err(spawn_error(redactor.redact(&e.to_string())));
                }
            },
        }
        if !open && exited.is_some() {
            break;
        }
    }
    held.finish(sink);
    let Some((status, _)) = exited else {
        return Ok((stop(&mut child), None));
    };
    let end = status.code().map_or(StreamEnd::Stopped, StreamEnd::Exited);
    Ok((end, Some(status)))
}
