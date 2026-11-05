//! Talking to the daemon.
//!
//! The command line is one client of the daemon among several, with no powers
//! the others lack. It sends one request, reads the steps as they happen, and
//! renders the answer. It never decides policy: everything it shows a person
//! was decided on the other side of the socket.

use std::io::{BufReader, BufWriter};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use proto::{ApiError, Envelope, ErrorCode, Frame, Outcome, OutputStream, Reply, Request, WireError};

/// Why a call did not produce an answer.
#[derive(Debug)]
pub enum CallError {
    /// Nothing is listening. This is worth its own case because the fix is
    /// different from every other failure.
    NotRunning { socket: PathBuf },
    /// The daemon answered, and the answer was no.
    Refused(ApiError),
    /// The connection itself failed.
    Transport(String),
    /// The time limit passed before the daemon finished answering.
    TimedOut,
    /// The caller stopped reading, so the call was ended.
    Stopped,
}

impl CallError {
    pub fn message(&self) -> String {
        match self {
            Self::NotRunning { socket } => format!(
                "the {} daemon is not running.\nStart it with {}, then try again.\nIt looks for its socket at {}",
                brand::PRODUCT_NAME,
                brand::DAEMON_BIN,
                crate::ui::esc_path(socket)
            ),
            // A daemon started before a request existed cannot parse it. The
            // words are the wire error's own, so this cannot match anything else.
            Self::Refused(error)
                if error.code == ErrorCode::BadRequest
                    && error.message == WireError::Malformed.to_string() =>
            {
                format!(
                    "the running daemon is older than this command, so it does not know this request. Restart {}, then try again. Its sandboxes keep running.",
                    brand::DAEMON_BIN
                )
            }
            Self::Refused(error) => error.message.clone(),
            Self::Transport(reason) => reason.clone(),
            Self::TimedOut => "the daemon did not answer in time".to_owned(),
            Self::Stopped => "stopped".to_owned(),
        }
    }
}

/// Somewhere to send a request. A trait so that a test can drive the real
/// daemon logic in process, without a socket.
pub trait DaemonClient {
    fn call(
        &self,
        request: Request,
        progress: &mut dyn FnMut(&str, Option<&str>),
    ) -> Result<Reply, CallError>;

    /// Sends one request and hands over its output as it arrives. Returning
    /// `false` from `output` ends the call, which also stops the command on
    /// the daemon's side. `limit` bounds the whole call, for a caller that must
    /// not be left waiting.
    fn stream(
        &self,
        request: Request,
        output: &mut dyn FnMut(OutputStream, &str) -> bool,
        limit: Option<Duration>,
    ) -> Result<Reply, CallError>;
}

/// The real client: an owner-only Unix socket in the state folder.
pub struct SocketClient {
    socket: PathBuf,
}

impl SocketClient {
    pub fn new(state: &Path) -> Self {
        Self {
            socket: state.join(brand::SOCKET_FILE),
        }
    }
}

impl SocketClient {
    fn exchange(
        &self,
        request: Request,
        progress: &mut dyn FnMut(&str, Option<&str>),
        output: &mut dyn FnMut(OutputStream, &str) -> bool,
        limit: Option<Duration>,
    ) -> Result<Reply, CallError> {
        let deadline = limit.map(|limit| Instant::now() + limit);
        let stream = UnixStream::connect(&self.socket).map_err(|_| CallError::NotRunning {
            socket: self.socket.clone(),
        })?;
        // A build can take minutes, so without a limit there is no short read
        // timeout here. A write that blocks forever is a different matter.
        let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
        let mut reader = BufReader::new(
            stream
                .try_clone()
                .map_err(|e| CallError::Transport(e.kind().to_string()))?,
        );
        let mut writer = BufWriter::new(stream.try_clone().map_err(|e| {
            CallError::Transport(e.kind().to_string())
        })?);
        proto::write_frame(&mut writer, &Envelope::new(request))
            .map_err(|e| CallError::Transport(e.to_string()))?;

        loop {
            if let Some(deadline) = deadline {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(CallError::TimedOut);
                }
                let _ = stream.set_read_timeout(Some(left));
            }
            match proto::read_frame::<_, Frame>(&mut reader) {
                Ok(Some(Frame::Progress { step, detail })) => {
                    progress(&step, detail.as_deref());
                }
                Ok(Some(Frame::Output { stream, text })) => {
                    if !output(stream, &text) {
                        return Err(CallError::Stopped);
                    }
                }
                Ok(Some(Frame::Done(Outcome::Ok { reply }))) => return Ok(reply),
                Ok(Some(Frame::Done(Outcome::Error { error }))) => {
                    return Err(CallError::Refused(error))
                }
                Ok(None) => {
                    return Err(CallError::Transport(
                        "the daemon closed the connection without answering".to_owned(),
                    ))
                }
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
}

impl DaemonClient for SocketClient {
    fn call(
        &self,
        request: Request,
        progress: &mut dyn FnMut(&str, Option<&str>),
    ) -> Result<Reply, CallError> {
        self.exchange(request, progress, &mut |_, _| true, None)
    }

    fn stream(
        &self,
        request: Request,
        output: &mut dyn FnMut(OutputStream, &str) -> bool,
        limit: Option<Duration>,
    ) -> Result<Reply, CallError> {
        self.exchange(request, &mut |_, _| {}, output, limit)
    }
}

/// Used when no daemon could be located at all, so that every command fails
/// the same clear way instead of each inventing its own message.
pub struct NoDaemon {
    pub reason: String,
}

impl DaemonClient for NoDaemon {
    fn call(
        &self,
        _request: Request,
        _progress: &mut dyn FnMut(&str, Option<&str>),
    ) -> Result<Reply, CallError> {
        Err(CallError::Transport(self.reason.clone()))
    }

    fn stream(
        &self,
        _request: Request,
        _output: &mut dyn FnMut(OutputStream, &str) -> bool,
        _limit: Option<Duration>,
    ) -> Result<Reply, CallError> {
        Err(CallError::Transport(self.reason.clone()))
    }
}
