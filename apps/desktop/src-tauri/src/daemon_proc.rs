//! Finding, starting and restarting the daemon.
//!
//! The daemon is its own process and outlives the app: it is started in its
//! own process group with no terminal, so quitting the app or closing the
//! terminal that launched it leaves it running. Nothing is passed to it on its
//! command line, so no secret can end up in a process listing.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::client::Client;

/// Where the daemon binary is. The copy shipped inside the app comes first,
/// so the app and the daemon it starts are the same build.
pub fn find_daemon(
    exe: Option<&Path>,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    find_beside(exe, brand::DAEMON_BIN).or_else(|| find_on_path(brand::DAEMON_BIN, env))
}

/// The command line tool shipped beside the app's own executable, if any.
pub fn bundled_cli(exe: Option<&Path>) -> Option<PathBuf> {
    find_beside(exe, brand::CLI_BIN)
}

fn find_beside(exe: Option<&Path>, name: &str) -> Option<PathBuf> {
    let candidate = exe?.parent()?.join(name);
    is_executable(&candidate).then_some(candidate)
}

fn find_on_path(name: &str, env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let path = env("PATH")?;
    std::env::split_paths(&path)
        // A relative entry would depend on whatever folder the app started in.
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The only variables the daemon is started with.
///
/// The app may have been launched with anything in its environment: cloud
/// keys, tokens, or a variable naming some other program as the engine. The
/// daemon holds the secrets and runs the engine, so it gets only what it
/// reads, and a fixed search path. It clears its children's environments
/// again on its own, so nothing here reaches a sandbox either way.
///
/// - `HOME`, and the variable that moves the state folder, to find its state.
/// - The variable naming the engine binary, so a person who set it by hand
///   gets the same engine as when they start the daemon themselves.
/// - `USER`, `LOGNAME`, `TMPDIR`, and on Linux the session bus and runtime
///   folder, which the system keychain needs.
pub fn daemon_env(env: &dyn Fn(&str) -> Option<OsString>) -> Vec<(String, OsString)> {
    let mut keep: Vec<String> = [
        "HOME",
        "USER",
        "LOGNAME",
        "TMPDIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "XDG_RUNTIME_DIR",
    ]
    .iter()
    .map(|k| (*k).to_owned())
    .collect();
    keep.push(hoststate::home_override_var());
    keep.push(hoststate::engine_bin_var());
    let mut vars: Vec<(String, OsString)> = keep
        .into_iter()
        .filter_map(|key| env(&key).map(|value| (key, value)))
        .collect();
    vars.push(("PATH".to_owned(), OsString::from("/usr/bin:/bin")));
    vars
}

/// Starts the daemon in the background and waits briefly for it to answer.
/// Its output goes to `log`, which is created owner-only.
pub fn spawn_daemon(binary: &Path, log: &Path, client: &Client) -> Result<(), String> {
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create the log folder: {}", e.kind()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(log)
        .map_err(|e| format!("cannot open the daemon log: {}", e.kind()))?;
    let err = file
        .try_clone()
        .map_err(|e| format!("cannot open the daemon log: {}", e.kind()))?;
    let mut child = Command::new(binary)
        .env_clear()
        .envs(daemon_env(&|key| std::env::var_os(key)))
        .stdin(Stdio::null())
        .stdout(file)
        .stderr(err)
        // Its own group, so a signal sent to the app's group does not reach it.
        .process_group(0)
        .spawn()
        .map_err(|e| format!("cannot start {}: {}", brand::DAEMON_BIN, e.kind()))?;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if client.ping().is_ok() {
            // Reaped in the background if it ever exits, so it never lingers
            // as a zombie while the app runs.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "{} stopped straight away ({}). Its log may say why: {}",
                brand::DAEMON_BIN,
                status,
                proto::text::escape_path(log)
            ));
        }
        if Instant::now() >= deadline {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return Err(format!(
                "{} started but did not answer within 5 seconds. Its log may say why: {}",
                brand::DAEMON_BIN,
                proto::text::escape_path(log)
            ));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// The process listening on the daemon's socket. Only a process of this
/// user can listen there, since the folder is owner-only.
fn listener_pid(socket: &Path) -> Result<i32, String> {
    let stream = std::os::unix::net::UnixStream::connect(socket)
        .map_err(|_| "no daemon is listening".to_owned())?;
    peer_pid(&stream)
}

#[cfg(target_os = "linux")]
fn peer_pid(stream: &std::os::unix::net::UnixStream) -> Result<i32, String> {
    use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
    getsockopt(stream, PeerCredentials)
        .map(|c| c.pid())
        .map_err(|e| format!("cannot tell which process is the daemon: {e}"))
}

#[cfg(target_os = "macos")]
fn peer_pid(stream: &std::os::unix::net::UnixStream) -> Result<i32, String> {
    use nix::sys::socket::{getsockopt, sockopt::LocalPeerPid};
    getsockopt(stream, LocalPeerPid)
        .map_err(|e| format!("cannot tell which process is the daemon: {e}"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peer_pid(_stream: &std::os::unix::net::UnixStream) -> Result<i32, String> {
    Err("this system cannot tell which process is the daemon".to_owned())
}

/// Asks the running daemon to stop, waits for its socket to go quiet, then
/// starts a new one. Sandboxes are separate processes and keep running.
pub fn restart_daemon(binary: &Path, log: &Path, client: &Client) -> Result<(), String> {
    if client.ping().is_ok() {
        let pid = listener_pid(client.socket())?;
        if pid <= 1 {
            return Err("the daemon's process id is not usable".to_owned());
        }
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGTERM)
            .map_err(|e| format!("cannot stop the daemon: {e}"))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while client.ping().is_ok() {
            if Instant::now() >= deadline {
                return Err("the daemon did not stop within 5 seconds".to_owned());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    spawn_daemon(binary, log, client)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_daemon_gets_only_the_variables_it_reads() {
        let host = |key: &str| -> Option<OsString> {
            match key {
                "HOME" => Some("/home/me".into()),
                "AWS_SECRET_ACCESS_KEY" => Some("leak-me".into()),
                "GITHUB_TOKEN" => Some("leak-me".into()),
                "PATH" => Some("/tmp/evil:/usr/bin".into()),
                k if k == hoststate::engine_bin_var() => Some("/opt/engine/msb".into()),
                _ => None,
            }
        };
        let vars = daemon_env(&host);
        let keys: Vec<&str> = vars.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"HOME"));
        assert!(keys.contains(&hoststate::engine_bin_var().as_str()));
        assert!(!keys.contains(&"AWS_SECRET_ACCESS_KEY"));
        assert!(!keys.contains(&"GITHUB_TOKEN"));
        assert!(vars.iter().all(|(_, v)| v != "leak-me"));
        // The app's own search path is never passed on.
        let path: Vec<&OsString> = vars.iter().filter(|(k, _)| k == "PATH").map(|(_, v)| v).collect();
        assert_eq!(path, vec![&OsString::from("/usr/bin:/bin")]);
    }

    #[test]
    fn the_copy_beside_the_app_comes_before_the_path() {
        let beside = tempfile::tempdir().unwrap();
        let on_path = tempfile::tempdir().unwrap();
        for dir in [beside.path(), on_path.path()] {
            let bin = dir.join(brand::DAEMON_BIN);
            std::fs::write(&bin, "#!/bin/sh\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = on_path.path().as_os_str().to_owned();
        let env = move |k: &str| (k == "PATH").then(|| path.clone());
        let exe = beside.path().join("app");
        assert_eq!(
            find_daemon(Some(&exe), &env),
            Some(beside.path().join(brand::DAEMON_BIN))
        );
        let elsewhere = tempfile::tempdir().unwrap();
        assert_eq!(
            find_daemon(Some(&elsewhere.path().join("app")), &env),
            Some(on_path.path().join(brand::DAEMON_BIN))
        );
    }

    #[test]
    fn a_relative_path_entry_is_never_searched() {
        let env = |k: &str| (k == "PATH").then(|| OsString::from("relative/dir:."));
        assert_eq!(find_on_path(brand::DAEMON_BIN, &env), None);
    }
}
