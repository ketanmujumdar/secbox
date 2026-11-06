//! The host daemon: the one place that holds secrets, policy and approvals.
//!
//! Every client — the command line, a terminal interface, a desktop app —
//! speaks the same [`proto`] API over an owner-only Unix socket, so none of
//! them has a power another lacks. [`Service`] is the whole of that API and
//! knows nothing about sockets, which is what lets it be driven straight from
//! a test with a fake engine.
//!
//! # What this trusts
//!
//! The host, the person at the keyboard, and the project's files only as far
//! as reading them as data. Everything inside a sandbox is treated as
//! compromised: its output is text to escape and show, never a fact to act on
//! and never a reason to widen a rule.

pub mod audit;
pub mod blocked;
pub mod buildhosts;
pub mod debug;
pub mod diff;
pub mod gitref;
pub mod gitrelay;
pub mod netallow;
pub mod mask;
pub mod plan;
pub mod remote;
pub mod run;
pub mod secret;
pub mod server;
pub mod settings;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use engine::Engine;
use hoststate::host::HostInfo;
use hoststate::StatePaths;
use proto::{ApiError, ErrorCode, Reply, Request};
use secrets::{ProjectId, SecretStore};

/// Steps reported while a long request runs. Clients may show them as they
/// arrive, so they must not carry anything secret.
pub trait Progress {
    fn step(&mut self, step: &str, detail: Option<&str>);
    /// Sends some already filtered output. Returns `false` once nobody is
    /// reading it.
    fn output(&mut self, _stream: proto::OutputStream, _text: &str) -> bool {
        true
    }
    /// Whoever asked has gone away, even if nothing has been sent to notice it.
    fn cancelled(&self) -> bool {
        false
    }
}

/// Throws progress away. Useful in tests and for requests nobody watches.
pub struct Quiet;

impl Progress for Quiet {
    fn step(&mut self, _step: &str, _detail: Option<&str>) {}
}

/// Everything the service needs from the world outside it. A test supplies a
/// fake engine, a fixed clock and a memory-backed secret store; the real
/// daemon supplies the engine adapter, the system clock and the OS keychain.
pub trait Deps: Send + Sync {
    fn engine(&self) -> &dyn Engine;
    fn now_unix(&self) -> u64;
    fn host_info(&self, disk_path: &Path) -> HostInfo;
    fn store(&self, project: &ProjectId) -> Box<dyn SecretStore>;
    /// Copies a project's files into `dest`, ready to be handed to a sandbox,
    /// leaving secrets behind. This is a seam so that the rules about what is
    /// left out live in one place and can be driven from a test.
    fn snapshot(
        &self,
        root: &Path,
        source: &proto::Source,
        dest: &Path,
    ) -> Result<proto::SnapshotPlan, String>;

    /// Fetches `git_ref` from an SSH remote into the bare repository at
    /// `mirror`, with the user's own `ssh` and, if given, their agent. Returns
    /// the full id of the commit fetched. The URL and ref were checked first.
    /// A seam, so the daemon's handling can be tested without a network.
    fn fetch_remote(
        &self,
        _mirror: &Path,
        _url: &str,
        _git_ref: &str,
        _agent_socket: Option<&Path>,
    ) -> Result<String, String> {
        Err("fetching from a Git remote is not available in this build".to_owned())
    }

    /// Starts `ssh` to one Git service of `url`, with the user's own `ssh`,
    /// config and agent, for the relay to pump. A seam, so the relay can be
    /// tested against a local repository.
    fn spawn_ssh(
        &self,
        _url: &str,
        _service: gitbroker::GitService,
        _git_protocol: Option<&str>,
        _agent_socket: Option<&Path>,
    ) -> Result<std::process::Child, String> {
        Err("reaching a Git remote is not available in this build".to_owned())
    }
}

/// One project, as the daemon sees it.
#[derive(Debug, Clone)]
pub struct Project {
    pub id: ProjectId,
    /// The canonical path on the host.
    pub root: PathBuf,
    /// This project's folder inside the state folder.
    pub dir: PathBuf,
    /// The sandbox this project owns.
    pub sandbox: engine::SandboxName,
}

pub struct Service {
    paths: StatePaths,
    deps: Arc<dyn Deps>,
    log: audit::Log,
}

impl Service {
    pub fn new(paths: StatePaths, deps: Arc<dyn Deps>) -> Result<Self, String> {
        hoststate::ensure_state(&paths)?;
        let log = audit::Log::in_state_dir(&paths.state);
        Ok(Self { paths, deps, log })
    }

    pub fn state_dir(&self) -> &Path {
        &self.paths.state
    }

    pub fn audit(&self) -> &audit::Log {
        &self.log
    }

    pub fn deps(&self) -> &dyn Deps {
        self.deps.as_ref()
    }

    pub fn record(&self, entry: audit::Entry) {
        // A failure to record is reported where it can be seen, but it must
        // not turn a finished action into a reported failure.
        if let Err(reason) = self.log.write(&entry) {
            eprintln!("cannot write the audit log: {reason}");
        }
    }

    /// Runs one request. This is the entire API.
    pub fn handle(&self, request: &Request, progress: &mut dyn Progress) -> Result<Reply, ApiError> {
        match request {
            Request::Plan(args) => {
                let project = self.open(&args.project)?;
                plan::build(self, &project, &args.source)
                    .map(|plan| Reply::Plan(Box::new(plan)))
            }
            Request::Up(args) => {
                let project = self.open(&args.project)?;
                run::up(self, &project, args, progress).map(Reply::Up)
            }
            Request::Down(args) => {
                let project = self.open(&args.project)?;
                run::down(self, &project, args.destroy, progress).map(Reply::Down)
            }
            Request::Remove(args) => {
                let project = self.known(&args.project)?;
                run::remove(self, &project, progress).map(Reply::Remove)
            }
            Request::ApproveHost(args) => {
                let project = self.open(&args.project)?;
                netallow::approve(self, &project, &args.host, progress).map(Reply::ApproveHost)
            }
            Request::Status(args) => {
                let project = self.open(&args.project)?;
                run::status(self, &project).map(Reply::Status)
            }
            Request::Logs(args) => {
                debug::check_logs_args(args)?;
                let project = self.open(&args.project)?;
                debug::logs(self, &project, args, progress).map(Reply::Logs)
            }
            Request::Terminal(args) => {
                debug::check_terminal_args(args)?;
                let project = self.open(&args.project)?;
                debug::terminal(self, &project, args).map(Reply::Terminal)
            }
            Request::Blocked(args) => {
                let project = self.open(&args.project)?;
                blocked::report(self, &project, args).map(Reply::Blocked)
            }
            Request::Fetch(args) => remote::fetch(self, args, progress).map(Reply::Fetch),
            Request::Git(args) => {
                let project = self.open(&args.project)?;
                gitrelay::handle(self, &project, args).map(Reply::Git)
            }
            Request::Diff(args) => {
                let project = self.open(&args.project)?;
                diff::diff(self, &project, progress).map(Reply::Diff)
            }
        }
    }

    /// Resolves a client's path to a project, and makes its state folder.
    ///
    /// The path is canonicalised here and the identity derived from the
    /// result, so a client cannot name a project it did not point at, and
    /// cannot supply an identity of its own.
    pub(crate) fn open(&self, path: &str) -> Result<Project, ApiError> {
        if path.is_empty() {
            return Err(bad("no project path was given"));
        }
        let given = Path::new(path);
        if !given.is_absolute() {
            return Err(bad("the project path must be absolute"));
        }
        let root = std::fs::canonicalize(given)
            .map_err(|e| not_found(format!("cannot open the project: {}", e.kind())))?;
        if !root.is_dir() {
            return Err(bad("the project path is not a directory"));
        }
        let id = hoststate::project_id(&root).map_err(internal)?;
        let dir = hoststate::prepare_project(&self.paths, &id).map_err(internal)?;
        let sandbox = sandbox_name(&id)?;
        // Noting the project means a client can list what this machine knows
        // about without being told, and it is where a later answer to "this
        // project moved" will have to start.
        hoststate::ProjectRecord::touch(&dir, &root, self.deps.now_unix()).map_err(internal)?;
        Ok(Project {
            id,
            root,
            dir,
            sandbox,
        })
    }
}

impl Service {
    /// Finds a project this machine already has a record of. Unlike `open`, it
    /// makes no record, and the project's folder need not exist any more.
    pub(crate) fn known(&self, path: &str) -> Result<Project, ApiError> {
        if path.is_empty() {
            return Err(bad("no project path was given"));
        }
        if !Path::new(path).is_absolute() {
            return Err(bad("the project path must be absolute"));
        }
        let canonical = std::fs::canonicalize(path)
            .ok()
            .map(|real| real.to_string_lossy().into_owned());
        let found = hoststate::known_projects(&self.paths)
            .into_iter()
            .find(|(_, record)| record.path == path || canonical.as_deref() == Some(record.path.as_str()));
        let Some((id, record)) = found else {
            return Err(not_found("this machine has no record of that project"));
        };
        let dir = self.paths.project(&id);
        let sandbox = sandbox_name(&id)?;
        Ok(Project {
            id,
            root: PathBuf::from(record.path),
            dir,
            sandbox,
        })
    }
}

/// Turns a project's identity into a name the engine will accept: lowercase
/// letters, digits and inner hyphens, at most 63 bytes.
///
/// The identity already ends in a hash, and that hash is what keeps two
/// projects apart, so anything cut here is cut from the front and the hash
/// always survives.
pub(crate) fn sandbox_name(id: &ProjectId) -> Result<engine::SandboxName, ApiError> {
    let mapped: String = id
        .as_str()
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect();
    let mut collapsed = String::with_capacity(mapped.len());
    for c in mapped.chars() {
        if c == '-' && collapsed.ends_with('-') {
            continue;
        }
        collapsed.push(c);
    }
    let trimmed = collapsed.trim_matches('-');
    let kept = if trimmed.len() > 63 {
        trimmed[trimmed.len() - 63..].trim_start_matches('-')
    } else {
        trimmed
    };
    // A name that is all punctuation leaves nothing usable, and an empty name
    // must never be sent to the engine.
    if kept.is_empty() {
        return Err(internal("the project's name has no usable characters"));
    }
    engine::SandboxName::new(kept).map_err(|e| internal(e.to_string()))
}

pub(crate) fn bad(message: impl AsRef<str>) -> ApiError {
    ApiError::new(ErrorCode::BadRequest, message)
}

pub(crate) fn not_found(message: impl AsRef<str>) -> ApiError {
    ApiError::new(ErrorCode::NotFound, message)
}

pub(crate) fn refused(message: impl AsRef<str>) -> ApiError {
    ApiError::new(ErrorCode::Refused, message)
}

pub(crate) fn wrong_state(message: impl AsRef<str>) -> ApiError {
    ApiError::new(ErrorCode::WrongState, message)
}

pub(crate) fn unsupported(message: impl AsRef<str>) -> ApiError {
    ApiError::new(ErrorCode::Unsupported, message)
}

pub(crate) fn engine_error(message: impl AsRef<str>) -> ApiError {
    ApiError::new(ErrorCode::Engine, message)
}

pub(crate) fn internal(message: impl AsRef<str>) -> ApiError {
    ApiError::new(ErrorCode::Internal, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(text: &str) -> ProjectId {
        ProjectId::new(text).unwrap()
    }

    #[test]
    fn a_project_identity_becomes_a_name_the_engine_accepts() {
        let name = sandbox_name(&id("demo_shop-ab12cd34")).unwrap();
        assert_eq!(name.as_str(), "demo-shop-ab12cd34");
    }

    #[test]
    fn an_overlong_identity_keeps_its_hash() {
        let long = format!("{}-ab12cd34", "n".repeat(100));
        let name = sandbox_name(&id(&long)).unwrap();
        assert!(name.as_str().ends_with("ab12cd34"));
        assert!(name.as_str().len() <= 63);
    }

    #[test]
    fn two_identities_that_differ_only_in_the_hash_stay_apart() {
        let a = sandbox_name(&id(&format!("{}-aaaaaaaa", "x".repeat(100)))).unwrap();
        let b = sandbox_name(&id(&format!("{}-bbbbbbbb", "x".repeat(100)))).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn punctuation_does_not_become_a_leading_or_doubled_hyphen() {
        let name = sandbox_name(&id("p.--my..project--ff00ff00")).unwrap();
        assert_eq!(name.as_str(), "p-my-project-ff00ff00");
    }
}
