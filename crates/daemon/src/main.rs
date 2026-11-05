//! The daemon process.
//!
//! It holds the secrets, the policy and the approvals, and it is the only
//! thing that talks to the engine. Clients reach it over an owner-only Unix
//! socket in the state folder; there is no network port.

use std::path::Path;
use std::sync::Arc;

use daemon::settings::UserSettings;
use daemon::{audit, server, Deps, Service};
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
        _root: &Path,
        _source: &proto::Source,
        _dest: &Path,
    ) -> Result<proto::SnapshotPlan, String> {
        // Wired to the git broker in the milestone that widens its exclusion
        // rules. Refusing is the only honest answer until then: a snapshot
        // that quietly copied everything would carry secrets into the VM.
        Err("taking a snapshot is not built yet".to_owned())
    }
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
    let binary = match env(&hoststate::engine_bin_var()) {
        Some(path) => std::path::PathBuf::from(path),
        None => return Err(format!(
            "no engine binary. Set {} to its absolute path.",
            hoststate::engine_bin_var()
        )),
    };
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
    server::serve(service, &listener);
    Ok(())
}
