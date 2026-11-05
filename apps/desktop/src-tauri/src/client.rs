//! Talking to the daemon over its owner-only Unix socket.
//!
//! This is a small client on `proto::wire` rather than the command line's own,
//! because that one lives in a crate that brings the engine, the policy engine
//! and the keychain with it, and because a followed log here has to be stopped
//! from another thread when the window closes it. Stopping is done by shutting
//! the socket, which also ends the command on the daemon's side.
//!
//! The app has no power the command line lacks: every call is one envelope,
//! and the daemon decides everything.

use std::io::{BufReader, BufWriter};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use proto::{ApiError, Envelope, ErrorCode, Frame, Outcome, OutputStream, Reply, Request, WireError};

/// Why a call did not produce an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// Nothing is listening on the socket.
    NotRunning { socket: PathBuf },
    /// The daemon answered, and the answer was no.
    Refused(ApiError),
    /// The connection itself failed.
    Transport(String),
    /// The time limit passed before the daemon finished answering.
    TimedOut,
    /// The call was stopped from this side.
    Stopped,
}

impl CallError {
    /// Words for a person. Every part that came from the daemon was already
    /// escaped there, and the window shows it as plain text either way.
    pub fn message(&self) -> String {
        match self {
            Self::NotRunning { socket } => format!(
                "The {} daemon is not running. Start it from the menu bar icon, or run {}. It looks for its socket at {}.",
                brand::PRODUCT_NAME,
                brand::DAEMON_BIN,
                proto::text::escape_path(socket)
            ),
            Self::Refused(error)
                if error.code == ErrorCode::BadRequest
                    && error.message == WireError::Malformed.to_string() =>
            {
                format!(
                    "The running daemon is older than this app, so it does not know this request. Restart {} from the menu bar icon. Its sandboxes keep running.",
                    brand::DAEMON_BIN
                )
            }
            Self::Refused(error) => error.message.clone(),
            Self::Transport(reason) => reason.clone(),
            Self::TimedOut => "The daemon did not answer in time.".to_owned(),
            Self::Stopped => "Stopped.".to_owned(),
        }
    }
}

/// A place to hand the connection to, so that another thread can stop a call
/// that would otherwise wait for ever, such as a followed log.
#[derive(Clone, Default)]
pub struct Stopper {
    inner: Arc<Mutex<StopState>>,
}

#[derive(Default)]
struct StopState {
    stream: Option<UnixStream>,
    stopped: bool,
}

impl Stopper {
    pub fn stop(&self) {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        state.stopped = true;
        if let Some(stream) = state.stream.take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    pub fn is_stopped(&self) -> bool {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).stopped
    }

    /// Returns false when the call was already stopped, before it connected.
    fn hold(&self, stream: &UnixStream) -> bool {
        let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if state.stopped {
            return false;
        }
        state.stream = stream.try_clone().ok();
        true
    }
}

/// The socket in a state folder.
#[derive(Debug, Clone)]
pub struct Client {
    socket: PathBuf,
}

impl Client {
    pub fn new(state: &Path) -> Self {
        Self {
            socket: state.join(brand::SOCKET_FILE),
        }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// One request, with its steps and output handed over as they arrive.
    /// Returning false from `output` ends the call.
    pub fn exchange(
        &self,
        request: Request,
        progress: &mut dyn FnMut(&str, Option<&str>),
        output: &mut dyn FnMut(OutputStream, &str) -> bool,
        limit: Option<Duration>,
        stopper: Option<&Stopper>,
    ) -> Result<Reply, CallError> {
        let deadline = limit.map(|limit| Instant::now() + limit);
        let stream = UnixStream::connect(&self.socket).map_err(|_| CallError::NotRunning {
            socket: self.socket.clone(),
        })?;
        if let Some(stopper) = stopper {
            if !stopper.hold(&stream) {
                return Err(CallError::Stopped);
            }
        }
        let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
        let transport = |e: std::io::Error| CallError::Transport(e.kind().to_string());
        let mut reader = BufReader::new(stream.try_clone().map_err(transport)?);
        let mut writer = BufWriter::new(stream.try_clone().map_err(transport)?);
        proto::write_frame(&mut writer, &Envelope::new(request))
            .map_err(|e| CallError::Transport(e.to_string()))?;

        let stopped = || stopper.is_some_and(Stopper::is_stopped);
        loop {
            if let Some(deadline) = deadline {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(CallError::TimedOut);
                }
                let _ = stream.set_read_timeout(Some(left));
            }
            match proto::read_frame::<_, Frame>(&mut reader) {
                Ok(Some(Frame::Progress { step, detail })) => progress(&step, detail.as_deref()),
                Ok(Some(Frame::Output { stream, text })) => {
                    if !output(stream, &text) {
                        let _ = reader.get_ref().shutdown(Shutdown::Both);
                        return Err(CallError::Stopped);
                    }
                }
                Ok(Some(Frame::Done(Outcome::Ok { reply }))) => return Ok(reply),
                Ok(Some(Frame::Done(Outcome::Error { error }))) => {
                    return Err(CallError::Refused(error))
                }
                Ok(None) if stopped() => return Err(CallError::Stopped),
                Ok(None) => {
                    return Err(CallError::Transport(
                        "The daemon closed the connection without answering.".to_owned(),
                    ))
                }
                Err(_) if stopped() => return Err(CallError::Stopped),
                Err(WireError::Io(error))
                    if deadline.is_some()
                        && matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                {
                    return Err(CallError::TimedOut)
                }
                Err(error) => return Err(CallError::Transport(error.to_string())),
            }
        }
    }

    /// A request whose output, if any, is not wanted.
    pub fn call(&self, request: Request, limit: Option<Duration>) -> Result<Reply, CallError> {
        self.exchange(request, &mut |_, _| {}, &mut |_, _| true, limit, None)
    }

    /// Whether something on the socket speaks this protocol. It sends a
    /// request the daemon must refuse, so nothing is touched, and any
    /// structured answer means a daemon is there.
    pub fn ping(&self) -> Result<(), CallError> {
        let probe = Request::Status(proto::ProjectArgs {
            project: String::new(),
        });
        match self.call(probe, Some(Duration::from_secs(3))) {
            Ok(_) => Ok(()),
            Err(CallError::Refused(error)) if error.code == ErrorCode::Version => {
                Err(CallError::Refused(error))
            }
            Err(CallError::Refused(_)) => Ok(()),
            Err(other) => Err(other),
        }
    }
}
