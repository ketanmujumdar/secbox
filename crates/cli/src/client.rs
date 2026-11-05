//! Talking to the daemon.
//!
//! The command line is one client of the daemon among several, with no powers
//! the others lack. It sends one request, reads the steps as they happen, and
//! renders the answer. It never decides policy: everything it shows a person
//! was decided on the other side of the socket.

use std::io::{BufReader, BufWriter};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use proto::{ApiError, Envelope, Frame, Outcome, Reply, Request};

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
            Self::Refused(error) => error.message.clone(),
            Self::Transport(reason) => reason.clone(),
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

impl DaemonClient for SocketClient {
    fn call(
        &self,
        request: Request,
        progress: &mut dyn FnMut(&str, Option<&str>),
    ) -> Result<Reply, CallError> {
        let stream = UnixStream::connect(&self.socket).map_err(|_| CallError::NotRunning {
            socket: self.socket.clone(),
        })?;
        // A build can take minutes, so there is no short read timeout here.
        // A write that blocks forever is a different matter.
        let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
        let mut reader = BufReader::new(
            stream
                .try_clone()
                .map_err(|e| CallError::Transport(e.kind().to_string()))?,
        );
        let mut writer = BufWriter::new(stream);
        proto::write_frame(&mut writer, &Envelope::new(request))
            .map_err(|e| CallError::Transport(e.to_string()))?;

        loop {
            match proto::read_frame::<_, Frame>(&mut reader) {
                Ok(Some(Frame::Progress { step, detail })) => {
                    progress(&step, detail.as_deref());
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
                Err(error) => return Err(CallError::Transport(error.to_string())),
            }
        }
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
}
