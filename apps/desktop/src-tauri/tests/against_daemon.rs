//! The window's operations, driven against the real daemon server on a real
//! owner-only socket, with the fake engine behind it. Nothing here boots a VM
//! or opens a window: it shows that the app asks the daemon for the right
//! things and renders the answers, not that anything is contained.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use daemon::{server, Deps, Service};
use desktop::client::{CallError, Client, Stopper};
use desktop::ops::{self, RowState, ShownPlans};
use engine::{Engine, FakeEngine, OutputStream as StreamKind};
use hoststate::host::{HostInfo, Hypervisor};
use hoststate::StatePaths;
use proto::{SnapshotCounts, SnapshotPlan, Source};
use secrets::{MemoryStore, ProjectId, SecretStore};

const GIB: u64 = 1 << 30;

struct TestDeps {
    engine: FakeEngine,
}

impl Deps for TestDeps {
    fn engine(&self) -> &dyn Engine {
        &self.engine
    }

    fn now_unix(&self) -> u64 {
        1_762_300_000
    }

    fn host_info(&self, _disk_path: &Path) -> HostInfo {
        HostInfo {
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
            hypervisor: Hypervisor::Available("test".to_owned()),
            cpus: Some(8),
            memory_total: Some(32 * GIB),
            memory_available: Some(24 * GIB),
            disk_free: Some(200 * GIB),
        }
    }

    fn store(&self, _project: &ProjectId) -> Box<dyn SecretStore> {
        Box::new(MemoryStore::default())
    }

    fn snapshot(&self, root: &Path, source: &Source, dest: &Path) -> Result<SnapshotPlan, String> {
        std::fs::create_dir(dest).map_err(|e| e.to_string())?;
        std::fs::copy(root.join("compose.yaml"), dest.join("compose.yaml")).map_err(|e| e.to_string())?;
        Ok(SnapshotPlan {
            revision: match source {
                Source::Commit { revision } => Some(revision.clone()),
                Source::WorkingTree => None,
            },
            counts: Some(SnapshotCounts {
                files: 1,
                total_bytes: 64,
            }),
            excluded: vec![],
            warnings: vec![],
        })
    }
}

const COMPOSE: &str = "\
services:
  web:
    image: web:1
    ports:
      - \"3000:3000\"
  api:
    image: api:1
    ports:
      - \"127.0.0.1:8080:8080\"
";

struct Running {
    _home: tempfile::TempDir,
    project: tempfile::TempDir,
    paths: StatePaths,
    client: Client,
    deps: Arc<TestDeps>,
}

impl Running {
    fn path(&self) -> String {
        std::fs::canonicalize(self.project.path())
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }
}

/// A daemon serving on a socket in a fresh state folder, as `main` does,
/// except for the engine and the secret store.
fn daemon() -> Running {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("compose.yaml"), COMPOSE).unwrap();
    std::fs::write(
        project.path().join(brand::CONFIG_FILE),
        "resources:\n  memory: 4G\n  cpus: 2\n  disk: 30G\n",
    )
    .unwrap();
    let paths = StatePaths {
        state: home.path().join(brand::STATE_DIR),
        overridden: true,
    };
    hoststate::ensure_state(&paths).unwrap();
    let deps = Arc::new(TestDeps {
        engine: FakeEngine::new(),
    });
    let service = Arc::new(Service::new(paths.clone(), Arc::clone(&deps) as Arc<dyn Deps>).unwrap());
    let listener = server::bind(&paths.state).unwrap();
    std::thread::spawn(move || server::serve(service, &listener));
    let client = Client::new(&paths.state);
    Running {
        _home: home,
        project,
        paths,
        client,
        deps,
    }
}

#[test]
fn with_no_daemon_every_call_says_it_is_not_running() {
    let home = tempfile::tempdir().unwrap();
    let client = Client::new(home.path());
    assert!(matches!(client.ping(), Err(CallError::NotRunning { .. })));
    let error = ops::status(&client, "/tmp").unwrap_err();
    assert!(error.message().contains(brand::DAEMON_BIN), "{}", error.message());
}

#[test]
fn a_daemon_is_found_and_a_project_joins_the_list_once_it_is_asked_about() {
    let d = daemon();
    d.client.ping().unwrap();
    assert!(ops::list_projects(&d.client, &d.paths, true).is_empty());

    let report = ops::status(&d.client, &d.path()).unwrap();
    let view = ops::status_view(&report);
    assert_eq!(view.state, RowState::None);

    let rows = ops::list_projects(&d.client, &d.paths, true);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].path, d.path());
    assert_eq!(rows[0].state, RowState::None);
    assert_eq!(rows[0].label, "No sandbox");
}

#[test]
fn start_needs_the_plan_that_was_shown_then_stop_and_destroy_work() {
    let d = daemon();
    let path = d.path();
    let plan = ops::plan(&d.client, &path).unwrap();
    let view = ops::plan_view(&plan);
    assert_eq!(view.services, vec!["web", "api"]);
    assert_eq!(view.ports.iter().map(|p| p.host_port).collect::<Vec<_>>(), vec![3000, 8080]);
    assert!(view.summary[0].starts_with("2 services from commit"));
    assert!(view.blockers.is_empty(), "{:?}", view.blockers);

    let shown = ShownPlans::default();
    // Nothing shown yet, so nothing may start.
    assert!(shown.take_matching(&path, &plan.digest, &[3000]).is_err());
    shown.remember(&path, &plan);
    shown.take_matching(&path, &plan.digest, &[3000]).unwrap();

    let mut steps = Vec::new();
    let message = ops::up(&d.client, &path, &plan.digest, vec![3000], &mut |step, _| {
        steps.push(step.to_owned())
    })
    .unwrap();
    assert!(message.starts_with("Started"), "{message}");
    assert!(message.contains("127.0.0.1:3000"), "{message}");
    assert!(!steps.is_empty());

    let view = ops::status_view(&ops::status(&d.client, &path).unwrap());
    assert_eq!(view.state, RowState::Running);
    assert_eq!(view.ports, vec!["127.0.0.1:3000"]);
    assert_eq!(view.resources.as_ref().unwrap().memory, "4 GB");

    assert_eq!(ops::down(&d.client, &path, false).unwrap(), ops::STOPPED);
    assert_eq!(ops::down(&d.client, &path, true).unwrap(), ops::DESTROYED);
    // Destroy is always on in the window, so it must say when nothing was there.
    assert_eq!(ops::down(&d.client, &path, true).unwrap(), ops::NO_SANDBOX);

    let removed = ops::remove(&d.client, &path).unwrap();
    assert!(removed.contains("Its folder was not touched"), "{removed}");
    assert!(std::path::Path::new(&path).is_dir());
    assert!(ops::remove(&d.client, &path).is_err());
}

#[test]
fn a_stale_digest_is_refused_by_the_daemon_too() {
    let d = daemon();
    let path = d.path();
    let error = ops::up(&d.client, &path, "not-the-digest", vec![], &mut |_, _| {}).unwrap_err();
    assert!(matches!(error, CallError::Refused(_)), "{error:?}");
}

fn start(d: &Running) {
    let plan = ops::plan(&d.client, &d.path()).unwrap();
    ops::up(&d.client, &d.path(), &plan.digest, vec![], &mut |_, _| {}).unwrap();
}

#[test]
fn logs_arrive_without_control_codes() {
    let d = daemon();
    start(&d);
    d.deps
        .engine
        .push_stream_output(StreamKind::Stdout, b"web  | hello \x1b[31mred\x1b[0m\n");
    let mut seen = String::new();
    let result = ops::logs(&d.client, &d.path(), None, false, &Stopper::default(), &mut |t| {
        seen.push_str(t);
        true
    });
    assert!(ops::logs_end(&result).is_none(), "{result:?}");
    assert!(seen.contains("hello"), "{seen:?}");
    assert!(!seen.contains('\u{1b}'), "{seen:?}");
}

#[test]
fn a_followed_log_stops_when_asked() {
    let d = daemon();
    start(&d);
    d.deps.engine.keep_streams_open();
    let stopper = Stopper::default();
    let other = stopper.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        other.stop();
    });
    let result = ops::logs(&d.client, &d.path(), Some("web".to_owned()), true, &stopper, &mut |_| true);
    assert!(matches!(result, Err(CallError::Stopped)), "{result:?}");
    assert!(ops::logs_end(&result).is_none());
}

#[test]
fn a_service_name_that_is_not_a_name_never_reaches_the_daemon() {
    let d = daemon();
    let result = ops::logs(&d.client, &d.path(), Some("--follow".to_owned()), false, &Stopper::default(), &mut |_| true);
    assert!(matches!(result, Err(CallError::Transport(_))), "{result:?}");
}

#[test]
fn blocked_traffic_says_what_it_does_not_cover() {
    let d = daemon();
    let view = ops::blocked_view(&ops::blocked(&d.client, &d.path()).unwrap());
    assert!(!view.recording);
    assert!(view.notes.iter().any(|n| n.text == ops::HOSTNAMES_ONLY));
    assert!(view.rows.is_empty());
}

#[test]
fn a_review_reaches_the_daemon_and_its_refusals_read_plainly() {
    let d = daemon();
    let error = ops::diff(&d.client, &d.path()).unwrap_err();
    assert!(error.message().contains("no sandbox"), "{}", error.message());
    start(&d);
    // The fake guest sends nothing, which is not a bundle.
    let error = ops::diff(&d.client, &d.path()).unwrap_err();
    assert!(error.message().contains("no bundle"), "{}", error.message());
}

/// The rule from the build plan: the product name appears in Rust only in the
/// brand crate. The main workspace's guard does not look in here.
#[test]
fn rust_code_here_uses_the_brand_constants() {
    fn scan(dir: &Path, hits: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path, hits);
            } else if path.extension().is_some_and(|e| e == "rs")
                && std::fs::read_to_string(&path)
                    .unwrap()
                    .to_lowercase()
                    .contains(brand::PRODUCT_NAME)
            {
                hits.push(path.display().to_string());
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut hits = Vec::new();
    for dir in ["src", "tests"] {
        scan(&root.join(dir), &mut hits);
    }
    scan_file(&root.join("build.rs"), &mut hits);
    assert!(hits.is_empty(), "hardcoded product name in: {hits:?}");

    fn scan_file(path: &Path, hits: &mut Vec<String>) {
        if std::fs::read_to_string(path)
            .unwrap()
            .to_lowercase()
            .contains(brand::PRODUCT_NAME)
        {
            hits.push(path.display().to_string());
        }
    }
}
