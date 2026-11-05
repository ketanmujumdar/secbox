//! `up`, `down` and `status`, driven through the command line into the real
//! daemon logic, with a fake engine underneath.
//!
//! Nothing here starts a VM. What it shows is that the command line asks the
//! daemon rather than deciding anything itself, that it will not forward a
//! port nobody approved, and that it says plainly what it does not know.

mod common;

use std::path::Path;
use std::sync::Arc;

use cli::{CallError, DaemonClient};
use common::Harness;
use engine::{Engine, FakeEngine};
use hoststate::host::{HostInfo, Hypervisor};
use hoststate::StatePaths;
use proto::{Reply, Request, SnapshotCounts, SnapshotPlan, Source};
use secrets::{MemoryStore, ProjectId, SecretStore};

const GIB: u64 = 1 << 30;

struct Facts {
    engine: FakeEngine,
}

impl daemon::Deps for Facts {
    fn engine(&self) -> &dyn Engine {
        &self.engine
    }

    fn now_unix(&self) -> u64 {
        1_762_300_000
    }

    fn host_info(&self, _disk_path: &Path) -> HostInfo {
        HostInfo {
            os: "linux".to_owned(),
            arch: "aarch64".to_owned(),
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

    fn snapshot(
        &self,
        _root: &Path,
        source: &Source,
        dest: &Path,
    ) -> Result<SnapshotPlan, String> {
        // The real snapshot creates its destination and refuses one that exists.
        std::fs::create_dir(dest).unwrap();
        std::fs::write(dest.join("compose.yaml"), b"services: {}\n").unwrap();
        Ok(SnapshotPlan {
            revision: match source {
                Source::Commit { revision } => Some(revision.clone()),
                Source::WorkingTree => None,
            },
            counts: Some(SnapshotCounts {
                files: 1,
                total_bytes: 20,
            }),
            excluded: vec![proto::ExcludedFile {
                path: ".env".to_owned(),
                reason: "EnvFile".to_owned(),
            }],
            warnings: Vec::new(),
        })
    }
}

/// Runs the daemon's own logic in this process, so the command line is
/// exercised against the real thing rather than a stub of it.
struct InProcess {
    service: daemon::Service,
}

impl DaemonClient for InProcess {
    fn call(
        &self,
        request: Request,
        progress: &mut dyn FnMut(&str, Option<&str>),
    ) -> Result<Reply, CallError> {
        struct Relay<'a>(&'a mut dyn FnMut(&str, Option<&str>));
        impl daemon::Progress for Relay<'_> {
            fn step(&mut self, step: &str, detail: Option<&str>) {
                (self.0)(step, detail);
            }
        }
        self.service
            .handle(&request, &mut Relay(progress))
            .map_err(CallError::Refused)
    }
}

const COMPOSE: &str = "\
services:
  web:
    build: ./web
    ports:
      - \"3000:3000\"
  api:
    image: api:1
    ports:
      - \"8080:8080\"
";

fn harness() -> Harness {
    let h = Harness::new();
    std::fs::write(h.project.path().join("compose.yaml"), COMPOSE).unwrap();
    std::fs::create_dir_all(h.project.path().join("web")).unwrap();
    std::fs::write(h.project.path().join("web/Dockerfile"), "FROM scratch\n").unwrap();
    std::fs::write(
        h.project.path().join(brand::CONFIG_FILE),
        "resources:\n  memory: 4G\n  cpus: 2\n  disk: 30G\n",
    )
    .unwrap();
    let facts = Arc::new(Facts {
        engine: FakeEngine::new(),
    });
    let service = daemon::Service::new(
        StatePaths {
            state: h.state(),
            overridden: true,
        },
        facts as Arc<dyn daemon::Deps>,
    )
    .unwrap();
    h.with_daemon(Box::new(InProcess { service }))
}

#[test]
fn up_shows_the_plan_before_it_starts_anything() {
    let h = harness();
    let done = h.run(&["up", "--plan-only"], "");
    assert_eq!(done.code, 0, "{}", done.all());
    let text = done.all();
    assert!(text.contains("compose, 2 services"), "{text}");
    assert!(text.contains("4.0 GB memory"), "{text}");
    assert!(text.contains("deny by default"), "{text}");
    assert!(text.contains("3000 -> 3000 in web"), "{text}");
    assert!(text.contains("8080 -> 8080 in api"), "{text}");
    assert!(text.contains("Nothing was started"), "{text}");
}

/// The engine spike could not show that a sandbox's rules change between
/// build and run, so the command line has to say that build hosts stay
/// reachable rather than implying they are dropped.
#[test]
fn up_says_the_build_hosts_outlive_the_build() {
    let h = harness();
    let text = h.run(&["up", "--plan-only"], "").all();
    assert!(
        text.contains("build hosts stay reachable after the build"),
        "{text}"
    );
}

#[test]
fn up_forwards_nothing_when_the_answer_is_no() {
    let h = harness();
    let done = h.run(&["up"], "n\n");
    assert_eq!(done.code, 0, "{}", done.all());
    let text = done.all();
    assert!(text.contains("No ports will be forwarded"), "{text}");
    assert!(!text.contains("Ready"), "{text}");
}

#[test]
fn up_forwards_the_ports_when_the_answer_is_yes() {
    let h = harness();
    let done = h.run(&["up"], "y\n");
    assert_eq!(done.code, 0, "{}", done.all());
    let text = done.all();
    assert!(text.contains("http://127.0.0.1:3000"), "{text}");
    assert!(text.contains("http://127.0.0.1:8080"), "{text}");
}

#[test]
fn up_refuses_a_port_the_project_does_not_publish() {
    let h = harness();
    let done = h.run(&["up", "--port", "5432"], "");
    assert_eq!(done.code, cli::exit::USAGE, "{}", done.all());
    assert!(done.err.contains("5432"), "{}", done.all());
    assert!(done.err.contains("3000"), "{}", done.all());
}

#[test]
fn up_reports_each_step_as_it_happens() {
    let h = harness();
    let text = h.run(&["up", "--port", "3000"], "").all();
    for step in ["planning", "sandbox", "snapshot", "docker", "build"] {
        assert!(text.contains(step), "missing {step} in {text}");
    }
}

#[test]
fn a_build_with_too_little_memory_stops_before_anything_runs() {
    let h = harness();
    std::fs::write(
        h.project.path().join(brand::CONFIG_FILE),
        "resources:\n  memory: 2G\n",
    )
    .unwrap();
    let done = h.run(&["up"], "y\n");
    assert_eq!(done.code, cli::exit::FAILED, "{}", done.all());
    let text = done.all();
    assert!(text.contains("cannot be started yet"), "{text}");
    assert!(text.contains("2G"), "{text}");
    assert!(text.contains("Nothing was started"), "{text}");
}

#[test]
fn choosing_both_a_commit_and_uncommitted_changes_is_a_usage_error() {
    let h = harness();
    let done = h.run(&["up", "--commit", "abc", "--include-uncommitted"], "");
    assert_eq!(done.code, cli::exit::USAGE, "{}", done.all());
}

#[test]
fn status_says_that_outbound_requests_are_not_recorded_rather_than_showing_none() {
    let h = harness();
    let text = h.run(&["status"], "").all();
    assert!(text.contains("not recorded on the host yet"), "{text}");
    assert!(!text.contains("0 blocked"), "{text}");
}

#[test]
fn status_reports_the_sandbox_after_it_is_started() {
    let h = harness();
    h.run(&["up", "--all-ports"], "");
    let text = h.run(&["status"], "").all();
    assert!(text.contains("running"), "{text}");
    assert!(text.contains("127.0.0.1:3000"), "{text}");
    assert!(text.contains("4.0 GB memory"), "{text}");
}

#[test]
fn down_stops_and_destroy_removes() {
    let h = harness();
    h.run(&["up", "--all-ports"], "");
    let stopped = h.run(&["down"], "");
    assert_eq!(stopped.code, 0, "{}", stopped.all());
    assert!(stopped.out.contains("Stopped"), "{}", stopped.all());
    // Worth saying: stopping is not a clean shutdown for what runs inside.
    assert!(stopped.out.contains("recover"), "{}", stopped.all());

    let destroyed = h.run(&["down", "--destroy"], "");
    assert_eq!(destroyed.code, 0, "{}", destroyed.all());
    assert!(destroyed.out.contains("Destroyed"), "{}", destroyed.all());
    assert!(
        destroyed.out.contains("Rotate every secret"),
        "{}",
        destroyed.all()
    );
}

#[test]
fn down_on_a_project_with_no_sandbox_says_so() {
    let h = harness();
    let done = h.run(&["down"], "");
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(done.out.contains("no sandbox"), "{}", done.all());
}
