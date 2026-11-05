//! The socket the clients talk to.
//!
//! # Who may connect
//!
//! Two things keep this to one person, and both have to hold:
//!
//! 1. The socket sits inside the state folder, which is `0700` and owned by
//!    the user. Reaching a Unix socket means traversing its directory, and
//!    directory permissions are enforced on every system this runs on. The
//!    socket itself is `0600` as well.
//! 2. Every connection's peer is asked for its user id, and one that does not
//!    match this process's own is closed without reading a byte.
//!
//! The first is the boundary. The second catches the cases the first does not
//! cover — a process running as root, or a state folder whose permissions
//! were loosened by hand — and makes the refusal visible in the log.
//!
//! There is no network port, by design: a browser cannot be made to reach a
//! Unix socket, so a compromised page on `localhost` has nothing to aim at.

use std::io::{BufReader, BufWriter, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use proto::{ApiError, Envelope, ErrorCode, Frame, Outcome, PROTOCOL_VERSION};

use crate::{audit, Progress, Service};

/// Where the socket lives inside the state folder.
pub fn socket_path(state: &Path) -> PathBuf {
    state.join(brand::SOCKET_FILE)
}

/// Reads the user id at the other end of a connection.
#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> Result<u32, String> {
    use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
    getsockopt(stream, PeerCredentials)
        .map(|creds| creds.uid())
        .map_err(|e| format!("cannot read who connected: {e}"))
}

#[cfg(target_os = "macos")]
fn peer_uid(stream: &UnixStream) -> Result<u32, String> {
    use nix::sys::socket::{getsockopt, sockopt::LocalPeerCred};
    getsockopt(stream, LocalPeerCred)
        .map(|cred| cred.uid())
        .map_err(|e| format!("cannot read who connected: {e}"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peer_uid(_stream: &UnixStream) -> Result<u32, String> {
    // Refusing is the only safe answer: without the check, the directory
    // permissions would be the only thing left, and this file promises two.
    Err("this system cannot report who connected".to_owned())
}

/// Binds the socket, owner-only, replacing one left behind by a daemon that
/// is no longer running.
pub fn bind(state: &Path) -> Result<UnixListener, String> {
    let path = socket_path(state);
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if meta.file_type().is_symlink() {
            return Err("the socket path is a symbolic link".to_owned());
        }
        if UnixStream::connect(&path).is_ok() {
            return Err("another daemon is already listening".to_owned());
        }
        std::fs::remove_file(&path)
            .map_err(|e| format!("cannot remove the old socket: {}", e.kind()))?;
    }
    let listener = UnixListener::bind(&path)
        .map_err(|e| format!("cannot open the socket: {}", e.kind()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("cannot tighten the socket: {}", e.kind()))?;
    Ok(listener)
}

/// Serves until the listener fails. One thread per connection, and one
/// request per connection.
pub fn serve(service: Arc<Service>, listener: &UnixListener) {
    let me = nix::unistd::Uid::current().as_raw();
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let service = Arc::clone(&service);
        std::thread::spawn(move || {
            if let Err(reason) = handle_connection(&service, stream, me) {
                eprintln!("connection closed: {reason}");
            }
        });
    }
}

fn handle_connection(service: &Service, stream: UnixStream, me: u32) -> Result<(), String> {
    match peer_uid(&stream) {
        Ok(uid) if uid == me => {}
        Ok(uid) => {
            service.record(
                audit::entry(service.deps().now_unix(), audit::Action::RequestRefused)
                    .detail(format!("a process owned by user {uid} tried to connect")),
            );
            return Err(format!("refused a connection from user {uid}"));
        }
        Err(reason) => {
            service.record(
                audit::entry(service.deps().now_unix(), audit::Action::RequestRefused)
                    .detail("the peer's owner could not be read"),
            );
            return Err(reason);
        }
    }
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut writer = BufWriter::new(stream);

    let envelope: Envelope = match proto::read_frame(&mut reader) {
        Ok(Some(envelope)) => envelope,
        Ok(None) => return Ok(()),
        Err(error) => {
            let api = ApiError::new(ErrorCode::BadRequest, error.to_string());
            return finish(&mut writer, Outcome::Error { error: api });
        }
    };
    if envelope.version != PROTOCOL_VERSION {
        let api = ApiError::new(
            ErrorCode::Version,
            format!(
                "this daemon speaks protocol {PROTOCOL_VERSION}, and the client speaks {}",
                envelope.version
            ),
        );
        return finish(&mut writer, Outcome::Error { error: api });
    }

    let mut progress = FrameProgress {
        writer: &mut writer,
        failed: false,
    };
    let outcome = match service.handle(&envelope.request, &mut progress) {
        Ok(reply) => Outcome::Ok { reply },
        Err(error) => Outcome::Error { error },
    };
    finish(&mut writer, outcome)
}

fn finish<W: Write>(writer: &mut W, outcome: Outcome) -> Result<(), String> {
    proto::write_frame(writer, &Frame::Done(outcome)).map_err(|e| e.to_string())
}

/// Sends each step to the client as it happens. A client that has gone away
/// stops the steps but not the work, which is already under way.
struct FrameProgress<'a, W: Write> {
    writer: &'a mut W,
    failed: bool,
}

impl<W: Write> Progress for FrameProgress<'_, W> {
    fn step(&mut self, step: &str, detail: Option<&str>) {
        if self.failed {
            return;
        }
        let frame = Frame::Progress {
            step: proto::text::escape_short(step, 120),
            detail: detail.map(|d| proto::text::escape_short(d, 200)),
        };
        if proto::write_frame(self.writer, &frame).is_err() {
            self.failed = true;
        }
    }
}
