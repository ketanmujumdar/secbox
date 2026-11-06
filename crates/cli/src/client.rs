//! Talking to the daemon.
//!
//! The command line is one client of the daemon among several, with no powers
//! the others lack. It sends one request, reads the steps as they happen, and
//! renders the answer. It never decides policy: everything it shows a person
//! was decided on the other side of the socket.

use std::io::{BufReader, BufWriter};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
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

    /// The message, safe to print on a terminal. The lines this program wrote
    /// itself keep their line breaks, and the one path in them is already
    /// escaped. Anything else came from the daemon or the system, so every
    /// control character is escaped, line breaks included, and it cannot
    /// fake a line of its own.
    pub fn for_terminal(&self) -> String {
        match self {
            Self::NotRunning { .. } => self.message(),
            _ => crate::ui::esc(&self.message()),
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

    /// Starts a request on a thread of its own and hands its output over as it
    /// arrives, for a caller that has to keep drawing and reading keys while a
    /// log is followed. `None` means this client cannot, and the caller should
    /// fall back to a call with a limit.
    fn background(&self, _request: Request) -> Option<Background> {
        None
    }
}

/// Something a background call has to say.
#[derive(Debug)]
pub enum Event {
    Output(OutputStream, String),
    /// The call is over. Nothing comes after this.
    Done(Result<Reply, CallError>),
}

/// How many pieces of output may wait to be taken. A sandbox that writes
/// faster than they are taken is held back, rather than filling memory.
const BACKGROUND_QUEUE: usize = 256;

/// A request running on another thread.
///
/// Dropping it ends the call at once. The connection is shut rather than left
/// to notice at the next line, because a quiet log can say nothing for hours,
/// and closing it is also what tells the daemon to stop the command.
pub struct Background {
    events: Receiver<Event>,
    stop: Option<Box<dyn FnOnce() + Send>>,
    over: bool,
}

impl Background {
    /// `stop` is run once, when this is dropped.
    pub fn new(events: Receiver<Event>, stop: impl FnOnce() + Send + 'static) -> Self {
        Self {
            events,
            stop: Some(Box::new(stop)),
            over: false,
        }
    }

    /// The next event, if one is waiting. This never waits.
    pub fn try_next(&mut self) -> Option<Event> {
        if self.over {
            return None;
        }
        match self.events.try_recv() {
            Ok(event) => {
                self.over = matches!(event, Event::Done(_));
                Some(event)
            }
            Err(TryRecvError::Empty) => None,
            // The thread went without saying how it ended.
            Err(TryRecvError::Disconnected) => {
                self.over = true;
                Some(Event::Done(Err(CallError::Transport(
                    "the connection to the daemon ended".to_owned(),
                ))))
            }
        }
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop();
        }
    }
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
    fn connect(&self) -> Result<UnixStream, CallError> {
        UnixStream::connect(&self.socket).map_err(|_| CallError::NotRunning {
            socket: self.socket.clone(),
        })
    }

    fn exchange(
        &self,
        request: Request,
        progress: &mut dyn FnMut(&str, Option<&str>),
        output: &mut dyn FnMut(OutputStream, &str) -> bool,
        limit: Option<Duration>,
    ) -> Result<Reply, CallError> {
        let stream = self.connect()?;
        converse(&stream, request, progress, output, limit)
    }
}

fn converse(
    stream: &UnixStream,
    request: Request,
    progress: &mut dyn FnMut(&str, Option<&str>),
    output: &mut dyn FnMut(OutputStream, &str) -> bool,
    limit: Option<Duration>,
) -> Result<Reply, CallError> {
    let deadline = limit.map(|limit| Instant::now() + limit);
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

    fn background(&self, request: Request) -> Option<Background> {
        let (sender, events) = mpsc::sync_channel(BACKGROUND_QUEUE);
        let connected = self.connect().and_then(|stream| {
            let shut = stream
                .try_clone()
                .map_err(|e| CallError::Transport(e.kind().to_string()))?;
            Ok((stream, shut))
        });
        let (stream, shut) = match connected {
            Ok(pair) => pair,
            Err(error) => {
                let _ = sender.send(Event::Done(Err(error)));
                return Some(Background::new(events, || {}));
            }
        };
        std::thread::spawn(move || {
            // A send fails only once the receiver is gone, and then nobody is
            // reading, so the call is ended.
            let done = converse(
                &stream,
                request,
                &mut |_, _| {},
                &mut |which, text| sender.send(Event::Output(which, text.to_owned())).is_ok(),
                None,
            );
            let _ = sender.send(Event::Done(done));
        });
        Some(Background::new(events, move || {
            let _ = shut.shutdown(Shutdown::Both);
        }))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixListener;

    #[test]
    fn not_running_keeps_its_own_line_breaks_on_a_terminal() {
        let error = CallError::NotRunning {
            socket: PathBuf::from(format!("/tmp/odd\u{1b}[2J/{}", brand::SOCKET_FILE)),
        };
        let text = error.for_terminal();
        assert_eq!(text.lines().count(), 3, "{text}");
        assert!(!text.contains("\\u{a}"), "{text}");
        assert!(text.contains("\\u{1b}[2J"), "the path is still escaped: {text}");
        assert!(!text.contains('\u{1b}'), "{text}");
    }

    #[test]
    fn text_from_the_daemon_cannot_fake_a_line_on_a_terminal() {
        let error = CallError::Refused(ApiError::new(
            ErrorCode::BadRequest,
            "refused\nStarted   in 1s\u{1b}[2J",
        ));
        let text = error.for_terminal();
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(text.contains("\\u{a}") && text.contains("\\u{1b}"), "{text}");
    }

    fn follow_request() -> Request {
        Request::Logs(proto::LogsArgs {
            project: "/p".to_owned(),
            service: None,
            tail: 10,
            follow: true,
        })
    }

    /// A followed log can be silent for hours, so dropping the call has to end
    /// it at once, without waiting for one more line, and the daemon has to
    /// see the connection close, which is what stops the command there.
    #[test]
    fn a_background_call_hands_over_output_and_ends_when_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(dir.path().join(brand::SOCKET_FILE)).unwrap();
        let (closed_tx, closed_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let envelope: Envelope = proto::read_frame(&mut reader).unwrap().unwrap();
            assert!(matches!(envelope.request, Request::Logs(_)));
            let mut writer = BufWriter::new(stream.try_clone().unwrap());
            proto::write_frame(
                &mut writer,
                &Frame::Output {
                    stream: OutputStream::Stdout,
                    text: "hello\n".to_owned(),
                },
            )
            .unwrap();
            drop(writer);
            // Then say nothing more, as a quiet log does, until the client goes.
            let mut rest = Vec::new();
            let _ = reader.read_to_end(&mut rest);
            closed_tx.send(()).unwrap();
        });

        let client = SocketClient::new(dir.path());
        let mut call = client.background(follow_request()).unwrap();
        let started = Instant::now();
        let first = loop {
            if let Some(event) = call.try_next() {
                break event;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "no output arrived");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(matches!(first, Event::Output(OutputStream::Stdout, ref t) if t == "hello\n"));
        assert!(call.try_next().is_none(), "nothing else was sent");

        drop(call);
        closed_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the connection was not closed");
        server.join().unwrap();
    }

    #[test]
    fn a_background_call_with_no_daemon_ends_with_the_usual_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut call = SocketClient::new(dir.path())
            .background(follow_request())
            .unwrap();
        assert!(matches!(
            call.try_next(),
            Some(Event::Done(Err(CallError::NotRunning { .. })))
        ));
        assert!(call.try_next().is_none(), "nothing comes after the end");
    }
}
