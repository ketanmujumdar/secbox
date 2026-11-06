//! The one hardened way to run git. Every git process in this crate is started
//! by [`GitCmd`].
//!
//! Options were checked against git 2.50.1 by experiment. Results that matter:
//!
//! - Under `protocol.allow=never`, fetching a bundle fails with
//!   `transport 'file' not allowed`, so bundle import needs [`Protocol::File`].
//! - `git bundle verify` passes a tree containing `..`. Only `fetch` with
//!   `fetch.fsckObjects` rejects it (`hasDotdot`), and the same fetch without
//!   fsck accepts it. Verify is therefore not a substitute for fsck.
//! - `-c` config does not reach `receive-pack` over the local transport, because
//!   git drops `GIT_CONFIG_PARAMETERS` there. `receive.fsckObjects` on this side
//!   does nothing for a push, and a local remote's own hooks do run.
//! - `-c core.hooksPath=/dev/null` stops hooks that plain git fires in the same
//!   repository, and it overrides a `core.hooksPath` in the repository's config.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::display::escape_stderr;
use crate::error::{Error, Result};

/// The only directories git may search, and the only places it is taken from.
const SAFE_PATH: &str = "/usr/bin:/bin";
const GIT_CANDIDATES: &[&str] = &[
    "/usr/bin/git",
    "/usr/local/bin/git",
    "/opt/homebrew/bin/git",
];

/// The oldest git these options were verified against. Older versions fail
/// closed, because fsck on bundle fetch depends on git's behavior.
const MIN_GIT: (u32, u32) = (2, 50);

pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
pub(crate) const NETWORK_TIMEOUT: Duration = Duration::from_secs(300);
const DEFAULT_MAX_STDOUT: u64 = 16 * 1024 * 1024;
const MAX_STDERR: usize = 64 * 1024;
const READER_GRACE: Duration = Duration::from_secs(5);

/// Config applied to every call. Command-line config beats repository config,
/// so these hold even in a repository whose config says otherwise.
pub(crate) const HARDENED_CONFIG: &[&str] = &[
    "core.hooksPath=/dev/null",
    "core.fsmonitor=false",
    "core.sshCommand=false",
    "core.attributesFile=/dev/null",
    "core.pager=cat",
    "core.askPass=",
    "core.protectHFS=true",
    "core.protectNTFS=true",
    "core.logAllRefUpdates=false",
    "credential.helper=",
    "diff.external=",
    "protocol.allow=never",
    "protocol.ext.allow=never",
    "transfer.fsckObjects=true",
    "fetch.fsckObjects=true",
    "receive.fsckObjects=true",
    "transfer.bundleURI=false",
    "fetch.recurseSubmodules=false",
    "submodule.recurse=false",
    "push.recurseSubmodules=no",
    "push.followTags=false",
    "push.gpgSign=false",
    "http.followRedirects=false",
    "gc.auto=0",
    "maintenance.auto=false",
];

/// Which transport one call may use. Everything else stays at `never`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Protocol {
    None,
    File,
    Https,
    /// SSH through the one command line in [`crate::SshTransport`]. Without
    /// one, `core.sshCommand` stays `false` and the call fails closed.
    Ssh,
}

pub(crate) struct GitOutput {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl GitOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    pub fn stdout_trimmed(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim().to_string()
    }
}

/// A single git invocation: fixed argv, no shell.
pub(crate) struct GitCmd {
    op: &'static str,
    git_dir: Option<PathBuf>,
    protocol: Protocol,
    extra_config: Vec<String>,
    args: Vec<OsString>,
    timeout: Duration,
    max_stdout: u64,
    stdout_file: Option<PathBuf>,
    ssh: Option<crate::remote::SshTransport>,
}

impl GitCmd {
    pub fn new(op: &'static str) -> Self {
        Self {
            op,
            git_dir: None,
            protocol: Protocol::None,
            extra_config: Vec::new(),
            args: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            max_stdout: DEFAULT_MAX_STDOUT,
            stdout_file: None,
            ssh: None,
        }
    }

    /// The SSH command and agent socket for a call with [`Protocol::Ssh`].
    pub fn ssh(mut self, transport: &crate::remote::SshTransport) -> Self {
        self.ssh = Some(transport.clone());
        self
    }

    /// Name the repository explicitly, so git never discovers one. The working
    /// directory of every call is the empty temporary `HOME`.
    pub fn git_dir(mut self, dir: &Path) -> Self {
        self.git_dir = Some(dir.to_path_buf());
        self
    }

    pub fn protocol(mut self, protocol: Protocol) -> Self {
        self.protocol = protocol;
        self
    }

    pub fn config(mut self, entry: impl Into<String>) -> Self {
        self.extra_config.push(entry.into());
        self
    }

    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|a| a.as_ref().to_os_string()));
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn max_stdout(mut self, limit: u64) -> Self {
        self.max_stdout = limit;
        self
    }

    /// Stream stdout into a file instead of memory. `max_stdout` still applies.
    pub fn stdout_file(mut self, path: &Path) -> Self {
        self.stdout_file = Some(path.to_path_buf());
        self
    }

    fn config_args(&self) -> Vec<String> {
        let mut all: Vec<String> = HARDENED_CONFIG.iter().map(|s| s.to_string()).collect();
        match self.protocol {
            Protocol::None => {}
            Protocol::File => all.push("protocol.file.allow=always".to_string()),
            Protocol::Https => all.push("protocol.https.allow=always".to_string()),
            Protocol::Ssh => {
                all.push("protocol.ssh.allow=always".to_string());
                // Later on the command line wins, so this replaces the
                // hardened `false` for this one call.
                if let Some(ssh) = &self.ssh {
                    all.push(format!("core.sshCommand={}", ssh.command_line()));
                }
            }
        }
        all.extend(self.extra_config.iter().cloned());
        all
    }

    fn command(&self, git: &Path, home: &Path) -> Command {
        let mut cmd = Command::new(git);
        cmd.env_clear()
            .env("PATH", SAFE_PATH)
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_PAGER", "cat")
            .env("LC_ALL", "C")
            .current_dir(home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        // The agent socket is the one thing from the user's environment that
        // may reach git, and only on a call that was given one.
        if self.protocol == Protocol::Ssh {
            if let Some(socket) = self.ssh.as_ref().and_then(|ssh| ssh.agent_socket()) {
                cmd.env("SSH_AUTH_SOCK", socket);
            }
        }
        for entry in self.config_args() {
            cmd.arg("-c").arg(entry);
        }
        if let Some(dir) = &self.git_dir {
            let mut flag = OsString::from("--git-dir=");
            flag.push(dir);
            cmd.arg(flag);
        }
        cmd.args(&self.args);
        cmd
    }

    /// Run and return whatever git did, including a non-zero exit.
    pub fn run(self) -> Result<GitOutput> {
        let git = git_binary()?;
        let home = tempfile::Builder::new().prefix("gb-home-").tempdir()?;
        let mut cmd = self.command(git, home.path());
        self.spawn_and_wait(&mut cmd)
    }

    /// Run and require exit status 0.
    pub fn ok(self) -> Result<GitOutput> {
        let op = self.op;
        let out = self.run()?;
        if out.success() {
            Ok(out)
        } else {
            Err(Error::GitFailed {
                op,
                code: out.code,
                stderr: escape_stderr(&out.stderr),
            })
        }
    }

    fn spawn_and_wait(&self, cmd: &mut Command) -> Result<GitOutput> {
        let mut child = cmd.spawn()?;
        let pid = child.id();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::GitUnavailable("no stdout pipe".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::GitUnavailable("no stderr pipe".to_string()))?;

        let overflow = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicUsize::new(0));

        let sink: Box<dyn Write + Send> = match &self.stdout_file {
            Some(path) => Box::new(File::create(path)?),
            None => Box::new(std::io::sink()),
        };
        let keep_in_memory = self.stdout_file.is_none();
        let limit = self.max_stdout;
        let out_reader = {
            let overflow = overflow.clone();
            let finished = finished.clone();
            thread::spawn(move || {
                let result = read_stdout(stdout, sink, keep_in_memory, limit, &overflow);
                finished.fetch_add(1, Ordering::SeqCst);
                result
            })
        };
        let err_reader = {
            let finished = finished.clone();
            thread::spawn(move || {
                let result = read_stderr(stderr);
                finished.fetch_add(1, Ordering::SeqCst);
                result
            })
        };

        let deadline = Instant::now() + self.timeout;
        let mut pause = Duration::from_millis(1);
        let status = loop {
            if overflow.load(Ordering::SeqCst) {
                terminate(&mut child, pid);
                return Err(Error::OutputTooLarge {
                    op: self.op,
                    limit: self.max_stdout,
                });
            }
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                terminate(&mut child, pid);
                return Err(Error::Timeout {
                    op: self.op,
                    seconds: self.timeout.as_secs(),
                });
            }
            thread::sleep(pause);
            pause = (pause * 2).min(Duration::from_millis(25));
        };

        // A helper that outlived git could hold the pipes open forever.
        let grace = Instant::now() + READER_GRACE;
        while finished.load(Ordering::SeqCst) < 2 && Instant::now() < grace {
            thread::sleep(Duration::from_millis(2));
        }
        if finished.load(Ordering::SeqCst) < 2 {
            kill_group(pid);
            return Err(Error::Timeout {
                op: self.op,
                seconds: READER_GRACE.as_secs(),
            });
        }
        let stdout = out_reader
            .join()
            .map_err(|_| Error::GitUnavailable("stdout reader panicked".to_string()))??;
        let stderr = err_reader.join().unwrap_or_default();
        if overflow.load(Ordering::SeqCst) {
            return Err(Error::OutputTooLarge {
                op: self.op,
                limit: self.max_stdout,
            });
        }
        Ok(GitOutput {
            code: status.code(),
            stdout,
            stderr,
        })
    }
}

fn read_stdout(
    mut pipe: impl Read,
    mut sink: Box<dyn Write + Send>,
    keep_in_memory: bool,
    limit: u64,
    overflow: &AtomicBool,
) -> std::io::Result<Vec<u8>> {
    let mut kept = Vec::new();
    let mut total: u64 = 0;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = pipe.read(&mut buf)?;
        if n == 0 {
            sink.flush()?;
            return Ok(kept);
        }
        total += n as u64;
        if total > limit {
            overflow.store(true, Ordering::SeqCst);
            return Ok(kept);
        }
        if keep_in_memory {
            kept.extend_from_slice(&buf[..n]);
        } else {
            sink.write_all(&buf[..n])?;
        }
    }
}

/// Keep reading past the cap, so a chatty git never blocks on a full pipe.
fn read_stderr(mut pipe: impl Read) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut buf = [0u8; 8 * 1024];
    while let Ok(n) = pipe.read(&mut buf) {
        if n == 0 {
            break;
        }
        let room = MAX_STDERR.saturating_sub(kept.len());
        kept.extend_from_slice(&buf[..n.min(room)]);
    }
    kept
}

fn terminate(child: &mut Child, pid: u32) {
    kill_group(pid);
    let _ = child.kill();
    let _ = child.wait();
}

/// Kill git and its helpers (index-pack, pack-objects, remote-https). The child
/// leads its own process group. `std` has no group kill, and this crate forbids
/// `unsafe`, so this uses the system `kill` with a fixed argv.
fn kill_group(pid: u32) {
    let _ = Command::new("/bin/kill")
        .env_clear()
        .args(["-KILL", "--", &format!("-{pid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn git_binary() -> Result<&'static Path> {
    static GIT: OnceLock<std::result::Result<PathBuf, (String, Option<String>)>> = OnceLock::new();
    let found = GIT.get_or_init(locate_git);
    match found {
        Ok(path) => Ok(path.as_path()),
        Err((found, None)) => Err(Error::GitUnavailable(found.clone())),
        Err((found, Some(required))) => Err(Error::GitTooOld {
            found: found.clone(),
            required: required.clone(),
        }),
    }
}

fn locate_git() -> std::result::Result<PathBuf, (String, Option<String>)> {
    let path = GIT_CANDIDATES
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .ok_or_else(|| ("no git binary in the fixed search list".to_string(), None))?;
    let output = Command::new(&path)
        .env_clear()
        .env("PATH", SAFE_PATH)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| (format!("could not run git: {e}"), None))?;
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    let version = parse_version(&text).ok_or_else(|| {
        (
            format!("unparseable version: {}", escape_stderr(text.as_bytes())),
            None,
        )
    })?;
    if version < MIN_GIT {
        return Err((
            format!("{}.{}", version.0, version.1),
            Some(format!("{}.{}", MIN_GIT.0, MIN_GIT.1)),
        ));
    }
    Ok(path)
}

fn parse_version(text: &str) -> Option<(u32, u32)> {
    let rest = text.strip_prefix("git version ")?;
    let mut parts = rest.split(|c: char| !c.is_ascii_digit());
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh_transport(dir: &Path) -> crate::remote::SshTransport {
        use std::os::unix::fs::PermissionsExt;
        let program = dir.join("ssh");
        std::fs::write(&program, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let socket = dir.join("agent");
        std::mem::forget(std::os::unix::net::UnixListener::bind(&socket).unwrap());
        crate::remote::SshTransport::new(&program, Some(&socket)).unwrap()
    }

    fn last_ssh_command(config: &[String]) -> Option<&str> {
        config
            .iter()
            .rev()
            .find_map(|entry| entry.strip_prefix("core.sshCommand="))
    }

    #[test]
    fn ssh_is_allowed_and_given_its_command_only_on_an_ssh_call() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = ssh_transport(dir.path());

        let call = GitCmd::new("fetch").protocol(Protocol::Ssh).ssh(&ssh);
        let config = call.config_args();
        assert!(config.contains(&"protocol.ssh.allow=always".to_string()));
        assert_eq!(last_ssh_command(&config), Some(ssh.command_line().as_str()));
        let command = call.command(Path::new("/usr/bin/git"), dir.path());
        let socket = command
            .get_envs()
            .find(|(key, _)| *key == "SSH_AUTH_SOCK")
            .and_then(|(_, value)| value);
        assert_eq!(socket, Some(dir.path().join("agent").as_os_str()));

        // Given a transport but not the protocol, nothing changes.
        let other = GitCmd::new("archive").protocol(Protocol::File).ssh(&ssh);
        assert_eq!(last_ssh_command(&other.config_args()), Some("false"));
        let command = other.command(Path::new("/usr/bin/git"), dir.path());
        assert!(command.get_envs().all(|(key, _)| key != "SSH_AUTH_SOCK"));

        // The protocol without a transport fails closed.
        let bare = GitCmd::new("fetch").protocol(Protocol::Ssh);
        assert_eq!(last_ssh_command(&bare.config_args()), Some("false"));
    }

    #[test]
    fn version_parsing() {
        assert_eq!(
            parse_version("git version 2.50.1 (Apple Git-155)\n"),
            Some((2, 50))
        );
        assert_eq!(parse_version("git version 2.9.0"), Some((2, 9)));
        assert_eq!(parse_version("nonsense"), None);
        assert!(parse_version("git version 2.9.0").unwrap() < MIN_GIT);
    }

    #[test]
    fn every_hardened_option_is_in_force() {
        let out = GitCmd::new("config")
            .args(["config", "--list", "--show-scope"])
            .ok()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
        for entry in HARDENED_CONFIG {
            let (key, value) = entry.split_once('=').unwrap();
            let expected = format!("command\t{}={}", key.to_lowercase(), value.to_lowercase());
            assert!(text.contains(&expected), "missing {expected}\n{text}");
        }
        for scope in ["system\t", "global\t"] {
            assert!(!text.contains(scope), "{scope} config loaded:\n{text}");
        }
    }

    #[test]
    fn environment_is_minimal_and_cwd_is_the_temporary_home() {
        let home = tempfile::tempdir().unwrap();
        let git = git_binary().unwrap();
        let cmd = GitCmd::new("x").command(git, home.path());
        let envs: Vec<(String, Option<String>)> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().to_string(),
                    v.map(|v| v.to_string_lossy().to_string()),
                )
            })
            .collect();
        let get = |name: &str| {
            envs.iter()
                .find(|(k, _)| k == name)
                .and_then(|(_, v)| v.clone())
        };
        assert_eq!(get("PATH").as_deref(), Some(SAFE_PATH));
        assert_eq!(get("GIT_CONFIG_NOSYSTEM").as_deref(), Some("1"));
        assert_eq!(get("GIT_CONFIG_GLOBAL").as_deref(), Some("/dev/null"));
        assert_eq!(get("GIT_TERMINAL_PROMPT").as_deref(), Some("0"));
        assert_eq!(get("GIT_ATTR_NOSYSTEM").as_deref(), Some("1"));
        assert_eq!(get("GIT_NO_REPLACE_OBJECTS").as_deref(), Some("1"));
        assert_eq!(get("GIT_DIR"), None);
        assert_eq!(cmd.get_current_dir(), Some(home.path()));
    }

    #[test]
    fn protocols_default_to_never_and_open_one_at_a_time() {
        let list = |p| GitCmd::new("x").protocol(p).config_args();
        assert!(list(Protocol::None)
            .iter()
            .all(|c| !c.ends_with("allow=always")));
        assert!(list(Protocol::File).contains(&"protocol.file.allow=always".to_string()));
        assert!(!list(Protocol::File).contains(&"protocol.https.allow=always".to_string()));
        assert!(list(Protocol::Https).contains(&"protocol.https.allow=always".to_string()));
        assert!(!list(Protocol::Https).contains(&"protocol.file.allow=always".to_string()));
    }

    #[test]
    fn ext_transport_cannot_run_a_command() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        let url = format!("ext::sh -c touch% {}", marker.display());
        let out = GitCmd::new("ls-remote")
            .args(["ls-remote", "--"])
            .arg(&url)
            .run()
            .unwrap();
        assert!(!out.success());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("not allowed"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!marker.exists());
    }

    #[test]
    fn timeout_kills_a_hanging_git() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("fifo");
        let made = Command::new("/usr/bin/mkfifo").arg(&fifo).status().unwrap();
        assert!(made.success());
        let start = Instant::now();
        let result = GitCmd::new("bundle")
            .args(["bundle", "list-heads"])
            .arg(&fifo)
            .timeout(Duration::from_millis(300))
            .run();
        assert!(matches!(result, Err(Error::Timeout { .. })));
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn stdout_cap_stops_git() {
        let result = GitCmd::new("config")
            .args(["config", "--list"])
            .max_stdout(8)
            .run();
        assert!(matches!(result, Err(Error::OutputTooLarge { .. })));
    }
}
