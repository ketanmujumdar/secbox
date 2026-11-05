//! The daemon's API, driven end to end against a fake engine.
//!
//! Nothing here boots a VM, so none of it is evidence that a project is
//! contained. What it does show is that the daemon asks the engine for the
//! right things, in the order the engine spike found works, and that it
//! refuses the things it promises to refuse.

use std::path::Path;
use std::sync::Arc;

use daemon::{Deps, Progress, Quiet, Service};
use engine::{Engine, FakeCall, FakeEngine};
use hoststate::host::{HostInfo, Hypervisor};
use hoststate::StatePaths;
use proto::{
    DownArgs, ErrorCode, PlanArgs, ProjectArgs, Reply, Request, SandboxState, SnapshotCounts,
    SnapshotPlan, Source, UpArgs,
};
use secrets::{MemoryStore, ProjectId, SecretStore};

const GIB: u64 = 1 << 30;

struct TestDeps {
    engine: FakeEngine,
    /// Set when the snapshot seam is asked to refuse, so the test can check
    /// that nothing is copied into a sandbox when it does.
    snapshot_fails: bool,
}

impl TestDeps {
    fn new() -> Self {
        Self {
            engine: FakeEngine::new(),
            snapshot_fails: false,
        }
    }
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
        root: &Path,
        source: &Source,
        dest: &Path,
    ) -> Result<SnapshotPlan, String> {
        if self.snapshot_fails {
            return Err("refused on purpose".to_owned());
        }
        // The real snapshot creates its destination and refuses one that exists.
        std::fs::create_dir(dest).expect("snapshot destination must not exist yet");
        std::fs::write(dest.join("compose.yaml"), std::fs::read(root.join("compose.yaml")).unwrap())
            .unwrap();
        Ok(SnapshotPlan {
            revision: match source {
                Source::Commit { revision } => Some(revision.clone()),
                Source::WorkingTree => None,
            },
            counts: Some(SnapshotCounts {
                files: 1,
                total_bytes: 64,
            }),
            excluded: vec![proto::ExcludedFile {
                path: ".env".to_owned(),
                reason: "environment file".to_owned(),
            }],
            warnings: Vec::new(),
        })
    }
}

struct Fixture {
    _state: tempfile::TempDir,
    project: tempfile::TempDir,
    service: Service,
    deps: Arc<TestDeps>,
}

/// A project that builds an image, because that is the case the memory floor
/// and the build host list exist for.
const COMPOSE: &str = "\
services:
  web:
    build: ./web
    ports:
      - \"3000:3000\"
  api:
    image: api:1
    ports:
      - \"127.0.0.1:8080:8080\"
";

fn fixture_with(compose: &str, config: Option<&str>) -> Fixture {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("compose.yaml"), compose).unwrap();
    std::fs::create_dir_all(project.path().join("web")).unwrap();
    std::fs::write(project.path().join("web/Dockerfile"), "FROM scratch\n").unwrap();
    if let Some(config) = config {
        std::fs::write(project.path().join(brand::CONFIG_FILE), config).unwrap();
    }
    let deps = Arc::new(TestDeps::new());
    let paths = StatePaths {
        state: state.path().join(brand::STATE_DIR),
        overridden: true,
    };
    let service = Service::new(paths, Arc::clone(&deps) as Arc<dyn Deps>).unwrap();
    Fixture {
        _state: state,
        project,
        service,
        deps,
    }
}

fn fixture() -> Fixture {
    fixture_with(COMPOSE, Some("resources:\n  memory: 4G\n  cpus: 2\n  disk: 30G\n"))
}

impl Fixture {
    fn path(&self) -> String {
        self.project.path().to_string_lossy().into_owned()
    }

    fn call(&self, request: Request) -> Result<Reply, proto::ApiError> {
        self.service.handle(&request, &mut Quiet)
    }

    fn plan(&self) -> proto::PlanReport {
        match self
            .call(Request::Plan(PlanArgs {
                project: self.path(),
                source: Source::default(),
            }))
            .unwrap()
        {
            Reply::Plan(plan) => *plan,
            other => panic!("unexpected {other:?}"),
        }
    }

    fn up(&self, ports: Vec<u16>) -> Result<proto::UpReport, proto::ApiError> {
        let plan = self.plan();
        match self.call(Request::Up(UpArgs {
            project: self.path(),
            source: Source::default(),
            plan_digest: plan.digest,
            approve_ports: ports,
        }))? {
            Reply::Up(report) => Ok(report),
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[test]
fn a_plan_describes_the_ports_the_project_publishes() {
    let f = fixture();
    let plan = f.plan();
    assert_eq!(plan.blockers, Vec::<String>::new());
    assert_eq!(plan.kind, "compose");
    assert_eq!(plan.services, vec!["web", "api"]);
    let ports: Vec<u16> = plan.ports.iter().map(|p| p.host_port).collect();
    assert_eq!(ports, vec![3000, 8080]);
    // The project asked for every interface on 3000 and loopback on 8080. Both
    // reach the host only on 127.0.0.1 either way.
    assert!(plan.ports[0].was_all_interfaces);
    assert!(!plan.ports[1].was_all_interfaces);
}

#[test]
fn a_plan_carries_the_build_hosts_and_no_approved_hosts_to_start_with() {
    let plan = fixture().plan();
    assert!(plan.network.build_hosts.contains(&"registry.npmjs.org".to_owned()));
    assert_eq!(plan.network.run_hosts, Vec::<String>::new());
    assert_eq!(plan.network.tls, proto::TlsPlan::ServerNameOnly);
    assert!(plan.network.merged_for_whole_life);
}

#[test]
fn the_same_project_plans_to_the_same_digest_and_a_change_moves_it() {
    let f = fixture();
    let first = f.plan().digest;
    assert_eq!(first, f.plan().digest);
    std::fs::write(
        f.project.path().join("compose.yaml"),
        COMPOSE.replace("3000:3000", "3100:3000"),
    )
    .unwrap();
    assert_ne!(first, f.plan().digest);
}

#[test]
fn a_build_that_would_get_too_little_memory_blocks_the_plan() {
    // 2G is the size the engine spike watched fail all three times.
    let f = fixture_with(COMPOSE, Some("resources:\n  memory: 2G\n"));
    let plan = f.plan();
    assert!(
        plan.blockers.iter().any(|b| b.contains("2G")),
        "{:?}",
        plan.blockers
    );
}

#[test]
fn starting_follows_the_sequence_the_engine_spike_found_works() {
    let f = fixture();
    let report = f.up(vec![3000]).unwrap();
    assert_eq!(report.sandbox, SandboxState::Running);
    assert_eq!(report.ports, vec![3000]);

    let calls = f.deps.engine.calls();
    let names: Vec<&str> = calls
        .iter()
        .map(|call| match call {
            FakeCall::Create(_) => "create",
            FakeCall::Start(_) => "start",
            FakeCall::Stop(_) => "stop",
            FakeCall::Destroy(_) => "destroy",
            FakeCall::Status(_) => "status",
            FakeCall::Exec(..) => "exec",
            FakeCall::ExecDetached(..) => "exec-detached",
            FakeCall::CopyIn(..) => "copy-in",
            FakeCall::Logs(..) => "logs",
        })
        .collect();
    assert_eq!(names[0], "status");
    assert_eq!(names[1], "create");
    assert_eq!(names[2], "copy-in");
    assert!(names.contains(&"exec"));

    // Only the approved port is forwarded, and it keeps its number.
    let FakeCall::Create(spec) = &calls[1] else {
        panic!("expected a create")
    };
    assert_eq!(spec.ports, vec![engine::HostPort(3000)]);
    assert_eq!(spec.image, "docker:dind");
}

#[test]
fn the_sandbox_is_created_at_the_larger_of_the_run_and_build_limits() {
    let f = fixture_with(
        COMPOSE,
        Some("resources:\n  memory: 4G\n  cpus: 2\n  disk: 30G\n  build:\n    memory: 6G\n"),
    );
    f.up(vec![]).unwrap();
    let calls = f.deps.engine.calls();
    let FakeCall::Create(spec) = &calls[1] else {
        panic!("expected a create")
    };
    // The policy cannot be changed later, so the peak has to be asked for up
    // front rather than raised when the build starts.
    assert_eq!(spec.resources.memory_mib, 6 * 1024);
    assert_eq!(spec.resources.cpus, 2);
    assert_eq!(spec.resources.disk_gib, 30);
}

#[test]
fn the_files_copied_in_are_the_snapshot_and_not_the_project_itself() {
    let f = fixture();
    std::fs::write(f.project.path().join(".env"), "SECRET=real\n").unwrap();
    f.up(vec![]).unwrap();
    let copied = f.deps.engine.copied(
        &engine::SandboxName::new(sandbox_name_of(&f)).unwrap(),
    );
    assert_eq!(copied.len(), 1);
    let (host, guest) = &copied[0];
    assert_eq!(guest.as_str(), "/work");
    // The staging directory is what was handed over, not the project folder.
    assert_ne!(host, f.project.path());
    assert!(!host.join(".env").exists());
}

fn sandbox_name_of(f: &Fixture) -> String {
    let id = hoststate::project_id(&std::fs::canonicalize(f.project.path()).unwrap()).unwrap();
    id.as_str()
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned()
}

#[test]
fn a_port_the_project_does_not_publish_cannot_be_approved() {
    let f = fixture();
    let error = f.up(vec![5432]).unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(error.message.contains("5432"));
    // Nothing was created.
    assert!(!f
        .deps
        .engine
        .calls()
        .iter()
        .any(|c| matches!(c, FakeCall::Create(_))));
}

#[test]
fn an_approval_for_a_plan_that_has_since_changed_is_refused() {
    let f = fixture();
    let stale = f.plan().digest;
    std::fs::write(
        f.project.path().join("compose.yaml"),
        COMPOSE.replace("3000:3000", "3100:3000"),
    )
    .unwrap();
    let error = f
        .call(Request::Up(UpArgs {
            project: f.path(),
            source: Source::default(),
            plan_digest: stale,
            approve_ports: vec![],
        }))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Refused);
    assert!(error.message.contains("changed"));
}

#[test]
fn a_project_that_is_not_compose_is_refused_rather_than_guessed_at() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("README.md"), "nothing here").unwrap();
    let service = Service::new(
        StatePaths {
            state: state.path().join(brand::STATE_DIR),
            overridden: true,
        },
        Arc::new(TestDeps::new()) as Arc<dyn Deps>,
    )
    .unwrap();
    let error = service
        .handle(
            &Request::Plan(PlanArgs {
                project: project.path().to_string_lossy().into_owned(),
                source: Source::default(),
            }),
            &mut Quiet,
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Unsupported);
}

#[test]
fn a_relative_or_missing_project_path_is_refused() {
    let f = fixture();
    for path in ["", "relative/path"] {
        let error = f
            .call(Request::Status(ProjectArgs {
                project: path.to_owned(),
            }))
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest, "{path:?}");
    }
    let error = f
        .call(Request::Status(ProjectArgs {
            project: "/nonexistent/project".to_owned(),
        }))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
}

#[test]
fn status_reports_nothing_before_anything_is_started() {
    let f = fixture();
    let Reply::Status(report) = f
        .call(Request::Status(ProjectArgs {
            project: f.path(),
        }))
        .unwrap()
    else {
        panic!("expected a status")
    };
    assert_eq!(report.sandbox, SandboxState::None);
    assert!(report.services.is_empty());
    assert!(report.resources.is_none());
}

#[test]
fn status_after_starting_reports_what_the_sandbox_was_given() {
    let f = fixture();
    f.up(vec![3000, 8080]).unwrap();
    let Reply::Status(report) = f
        .call(Request::Status(ProjectArgs {
            project: f.path(),
        }))
        .unwrap()
    else {
        panic!("expected a status")
    };
    assert_eq!(report.sandbox, SandboxState::Running);
    assert_eq!(report.ports, vec![3000, 8080]);
    assert_eq!(report.resources.unwrap().memory_bytes, 4 * GIB);
    // Nothing records outbound decisions yet, so this must not read as "none
    // were blocked".
    assert_eq!(report.blocked_count, 0);
}

#[test]
fn stopping_asks_the_workload_to_stop_before_the_sandbox() {
    let f = fixture();
    f.up(vec![]).unwrap();
    let before = f.deps.engine.calls().len();
    let Reply::Down(report) = f
        .call(Request::Down(DownArgs {
            project: f.path(),
            destroy: false,
        }))
        .unwrap()
    else {
        panic!("expected a down")
    };
    assert_eq!(report.sandbox, SandboxState::Stopped);
    assert!(!report.destroyed);

    let calls = f.deps.engine.calls();
    let after = &calls[before..];
    let compose_stop = after.iter().position(|c| {
        matches!(c, FakeCall::Exec(_, request) if request.command.contains(&"stop".to_owned()))
    });
    let sandbox_stop = after
        .iter()
        .position(|c| matches!(c, FakeCall::Stop(_)));
    // The engine spike saw Postgres run crash recovery when the sandbox was
    // stopped under a live stack, so the order here matters.
    assert!(
        compose_stop.is_some() && compose_stop < sandbox_stop,
        "{after:?}"
    );
}

#[test]
fn destroying_removes_the_sandbox_and_forgets_what_it_was_given() {
    let f = fixture();
    f.up(vec![3000]).unwrap();
    let Reply::Down(report) = f
        .call(Request::Down(DownArgs {
            project: f.path(),
            destroy: true,
        }))
        .unwrap()
    else {
        panic!("expected a down")
    };
    assert!(report.destroyed);
    assert_eq!(report.sandbox, SandboxState::None);

    let Reply::Status(status) = f
        .call(Request::Status(ProjectArgs {
            project: f.path(),
        }))
        .unwrap()
    else {
        panic!("expected a status")
    };
    assert!(status.resources.is_none());
    assert!(status.ports.is_empty());
}

#[test]
fn stopping_a_project_that_was_never_started_is_not_an_error() {
    let f = fixture();
    let Reply::Down(report) = f
        .call(Request::Down(DownArgs {
            project: f.path(),
            destroy: false,
        }))
        .unwrap()
    else {
        panic!("expected a down")
    };
    assert_eq!(report.sandbox, SandboxState::None);
}

#[test]
fn a_snapshot_that_cannot_be_taken_stops_the_start() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("compose.yaml"), COMPOSE).unwrap();
    std::fs::create_dir_all(project.path().join("web")).unwrap();
    std::fs::write(project.path().join("web/Dockerfile"), "FROM scratch\n").unwrap();
    std::fs::write(
        project.path().join(brand::CONFIG_FILE),
        "resources:\n  memory: 4G\n",
    )
    .unwrap();
    let deps = Arc::new(TestDeps {
        engine: FakeEngine::new(),
        snapshot_fails: true,
    });
    let service = Service::new(
        StatePaths {
            state: state.path().join(brand::STATE_DIR),
            overridden: true,
        },
        Arc::clone(&deps) as Arc<dyn Deps>,
    )
    .unwrap();
    let path = project.path().to_string_lossy().into_owned();
    let Reply::Plan(plan) = service
        .handle(
            &Request::Plan(PlanArgs {
                project: path.clone(),
                source: Source::default(),
            }),
            &mut Quiet,
        )
        .unwrap()
    else {
        panic!("expected a plan")
    };
    let error = service
        .handle(
            &Request::Up(UpArgs {
                project: path,
                source: Source::default(),
                plan_digest: plan.digest,
                approve_ports: vec![],
            }),
            &mut Quiet,
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Internal);
    // Nothing was copied into the sandbox.
    assert!(!deps
        .engine
        .calls()
        .iter()
        .any(|c| matches!(c, FakeCall::CopyIn(..))));
    // The sandbox is gone, so the next start can't reuse it with no code in it.
    assert!(deps
        .engine
        .calls()
        .iter()
        .any(|c| matches!(c, FakeCall::Destroy(..))));
}

#[test]
fn every_step_of_a_start_is_reported_to_whoever_is_watching() {
    struct Steps(Vec<String>);
    impl Progress for Steps {
        fn step(&mut self, step: &str, _detail: Option<&str>) {
            self.0.push(step.to_owned());
        }
    }
    let f = fixture();
    let plan = f.plan();
    let mut steps = Steps(Vec::new());
    f.service
        .handle(
            &Request::Up(UpArgs {
                project: f.path(),
                source: Source::default(),
                plan_digest: plan.digest,
                approve_ports: vec![],
            }),
            &mut steps,
        )
        .unwrap();
    assert_eq!(
        steps.0,
        vec!["planning", "sandbox", "snapshot", "docker", "build"]
    );
}

#[test]
fn what_happened_is_written_to_the_hosts_own_record() {
    let f = fixture();
    f.up(vec![3000]).unwrap();
    let text = std::fs::read_to_string(f.service.audit().path()).unwrap();
    assert!(text.contains("sandbox_created"));
    assert!(text.contains("port_forwarded"));
    // 8080 was in the plan and was not approved, which is worth recording.
    assert!(text.contains("port_refused"));
    assert!(text.contains("snapshot_excluded"));
    for line in text.lines() {
        serde_json::from_str::<serde_json::Value>(line).expect("every entry is one JSON line");
    }
}
