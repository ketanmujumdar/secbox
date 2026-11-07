//! The daemon process.
//!
//! It holds the secrets, the policy and the approvals, and it is the only
//! thing that talks to the engine. Clients reach it over an owner-only Unix
//! socket in the state folder; there is no network port.

use std::path::Path;
use std::sync::Arc;

use daemon::settings::UserSettings;
use daemon::{audit, gitref, server, Deps, Service};
use engine::{Engine, MsbEngine, MsbLocation};
use hoststate::host::HostInfo;
use hoststate::StatePaths;
use secrets::{KeyringStore, ProjectId, SecretStore};

struct RealDeps {
    engine: Box<dyn Engine>,
}

impl Deps for RealDeps {
    fn engine(&self) -> &dyn Engine {
        self.engine.as_ref()
    }

    fn now_unix(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }

    fn host_info(&self, disk_path: &Path) -> HostInfo {
        hoststate::host::probe(disk_path)
    }

    fn store(&self, project: &ProjectId) -> Box<dyn SecretStore> {
        Box::new(KeyringStore::new(project.clone()))
    }

    fn snapshot(
        &self,
        root: &Path,
        source: &proto::Source,
        dest: &Path,
    ) -> Result<proto::SnapshotPlan, String> {
        // The broker takes a full commit id and nothing else, so that a name
        // can never resolve to something other than what the person meant.
        let commit = match source {
            proto::Source::Commit { revision } if gitref::is_commit_id(revision) => {
                revision.clone()
            }
            proto::Source::Commit { revision } if revision == "HEAD" => {
                gitref::head_commit(root).map_err(|e| e.to_string())?
            }
            proto::Source::Commit { .. } => {
                return Err("name a commit by its full id, or use HEAD".to_owned())
            }
            proto::Source::WorkingTree => {
                return Err(
                    "including uncommitted changes is not built yet: commit first, or start from HEAD"
                        .to_owned(),
                )
            }
        };
        let report = gitbroker::snapshot(root, &commit, dest).map_err(|e| e.to_string())?;
        Ok(proto::SnapshotPlan {
            revision: Some(commit),
            counts: Some(proto::SnapshotCounts {
                files: report.files_written + report.symlinks_written,
                total_bytes: total_bytes(dest),
            }),
            excluded: report
                .excluded
                .iter()
                .map(|entry| proto::ExcludedFile {
                    path: entry.path.clone(),
                    reason: format!("{:?}", entry.reason),
                })
                .collect(),
            warnings: report
                .warnings
                .iter()
                .map(|w| {
                    format!(
                        "{} line {} looks like it holds a {:?}",
                        w.path, w.line, w.rule
                    )
                })
                .chain(
                    (!report.scan_complete)
                        .then(|| {
                            "the scan for secrets inside files stopped at its limit, so this list is not complete"
                                .to_owned()
                        }),
                )
                .collect(),
        })
    }

    fn person_check(&self) -> Option<&'static str> {
        personcheck::method()
    }

    fn confirm(&self, reason: &str) -> personcheck::Outcome {
        personcheck::confirm(reason)
    }

    fn fetch_remote(
        &self,
        mirror: &Path,
        url: &str,
        git_ref: &str,
        agent_socket: Option<&Path>,
    ) -> Result<String, String> {
        let program = gitbroker::SshTransport::find_program()
            .ok_or("no ssh program was found in /usr/bin, /usr/local/bin or /opt/homebrew/bin")?;
        let agent = Self::agent(agent_socket);
        let ssh = gitbroker::SshTransport::new(&program, agent.as_deref()).map_err(|e| e.to_string())?;
        gitbroker::fetch_ssh(mirror, url, git_ref, &ssh).map_err(|e| e.to_string())
    }

    fn spawn_ssh(
        &self,
        url: &str,
        service: gitbroker::GitService,
        git_protocol: Option<&str>,
        agent_socket: Option<&Path>,
    ) -> Result<std::process::Child, String> {
        let program = gitbroker::SshTransport::find_program()
            .ok_or("no ssh program was found in /usr/bin, /usr/local/bin or /opt/homebrew/bin")?;
        let agent = Self::agent(agent_socket);
        let ssh = gitbroker::SshTransport::new(&program, agent.as_deref()).map_err(|e| e.to_string())?;
        ssh.spawn_service(url, service, git_protocol).map_err(|e| e.to_string())
    }
}

impl RealDeps {
    /// The agent the client named, or else the one this daemon was started
    /// with, if it is a socket of this user. With neither, `ssh` falls back to
    /// its own key files, which works only for a key with no passphrase.
    fn agent(given: Option<&Path>) -> Option<std::path::PathBuf> {
        given.map(Path::to_path_buf).or_else(|| {
            std::env::var_os("SSH_AUTH_SOCK")
                .and_then(|path| daemon::remote::usable_agent_socket(Path::new(&path)))
        })
    }
}

/// Adds up what the snapshot holds, for the report. Bounded, because the
/// answer is only ever shown to a person.
fn total_bytes(dest: &Path) -> u64 {
    fn walk(dir: &Path, total: &mut u64, seen: &mut u64) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if *seen > 200_000 {
                return;
            }
            *seen += 1;
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                walk(&entry.path(), total, seen);
            } else if meta.is_file() {
                *total = total.saturating_add(meta.len());
            }
        }
    }
    let mut total = 0;
    let mut seen = 0;
    walk(dest, &mut total, &mut seen);
    total
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(reason) => {
            eprintln!("{}: {reason}", brand::DAEMON_BIN);
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let env = |key: &str| std::env::var_os(key);
    let paths = StatePaths::locate(&env)?;
    hoststate::ensure_state(&paths)?;

    let home = paths.state.join(hoststate::ENGINE_DIR);
    // The state folder belongs to this user, and the same place is what the
    // doctor command checks. Nothing searches the path or the current folder.
    let binary = match env(&hoststate::engine_bin_var()) {
        Some(path) => std::path::PathBuf::from(path),
        None => home.join("bin").join("msb"),
    };
    if !binary.is_file() {
        return Err(format!(
            "no engine binary at {}. Put it there, or set {} to its absolute path.",
            proto::text::escape_path(&binary),
            hoststate::engine_bin_var()
        ));
    }
    let engine = MsbEngine::new(MsbLocation::Explicit {
        binary,
        home: home.clone(),
    })
    .map_err(|e| e.to_string())?;
    engine.preflight().map_err(|e| e.to_string())?;

    // Reading the settings here means a damaged file stops the daemon rather
    // than being noticed later, in the middle of starting something.
    UserSettings::load(&paths.state)?.limits()?;

    let deps = Arc::new(RealDeps {
        engine: Box::new(engine),
    });
    let service = Arc::new(Service::new(paths.clone(), deps)?);
    let listener = server::bind(&paths.state)?;
    service.record(audit::entry(
        service.deps().now_unix(),
        audit::Action::DaemonStarted,
    ));
    eprintln!(
        "{} listening on {}",
        brand::DAEMON_BIN,
        server::socket_path(&paths.state).display()
    );
    let watching = Arc::clone(&service);
    std::thread::spawn(move || daemon::blocked::watch(watching));
    server::serve(service, &listener);
    Ok(())
}
