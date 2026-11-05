//! The daemon's API, driven end to end against a fake engine.
//!
//! Nothing here boots a VM, so none of it is evidence that a project is
//! contained. What it does show is that the daemon asks the engine for the
//! right things, in the order the engine spike found works, and that it
//! refuses the things it promises to refuse.

use std::path::Path;
use std::sync::Arc;

use daemon::{Deps, Progress, Quiet, Service};
use engine::{Engine, EngineLocation, FakeCall, FakeEngine, OutputStream as StreamKind};
use hoststate::host::{HostInfo, Hypervisor};
use hoststate::StatePaths;
use proto::{
    DownArgs, ErrorCode, LogsArgs, OutputStream, PlanArgs, ProjectArgs, Reply, Request,
    SandboxState, SnapshotCounts, SnapshotPlan, Source, TerminalArgs, UpArgs,
};
use secrets::{MemoryStore, ProjectId, SecretStore};

const GIB: u64 = 1 << 30;

struct TestDeps {
    engine: FakeEngine,
    /// Set when the snapshot seam is asked to refuse, so the test can check
    /// that nothing is copied into a sandbox when it does.
    snapshot_fails: bool,
    /// A file the snapshot has that the working tree does not.
    snapshot_adds: Option<(&'static str, &'static str)>,
}

impl TestDeps {
    fn new() -> Self {
        Self {
            engine: FakeEngine::new(),
            snapshot_fails: false,
            snapshot_adds: None,
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
        // The project's top-level files, leaving out environment files.
        for entry in std::fs::read_dir(root).unwrap().flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with(".env") || !entry.file_type().unwrap().is_file() {
                continue;
            }
            std::fs::copy(entry.path(), dest.join(&name)).unwrap();
        }
        if let Some((name, body)) = self.snapshot_adds {
            // As a commit snapshot could, when the working tree differs.
            std::fs::write(dest.join(name), body).unwrap();
        }
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
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("compose.yaml"), compose).unwrap();
    std::fs::create_dir_all(project.path().join("web")).unwrap();
    std::fs::write(project.path().join("web/Dockerfile"), "FROM scratch\n").unwrap();
    if let Some(config) = config {
        std::fs::write(project.path().join(brand::CONFIG_FILE), config).unwrap();
    }
    fixture_from(project, TestDeps::new())
}

fn fixture_from(project: tempfile::TempDir, deps: TestDeps) -> Fixture {
    let state = tempfile::tempdir().unwrap();
    let deps = Arc::new(deps);
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
    // Its only Dockerfile is FROM scratch: image hosts, and nothing that
    // takes uploads from anyone.
    assert!(plan.network.build_hosts.contains(&"registry-1.docker.io".to_owned()));
    assert!(!plan.network.build_hosts.contains(&"registry.npmjs.org".to_owned()));
    assert!(!plan.network.build_hosts.contains(&"storage.googleapis.com".to_owned()));
    assert_eq!(plan.network.run_hosts, Vec::<String>::new());
    assert_eq!(plan.network.tls, proto::TlsPlan::ServerNameOnly);
    assert!(plan.network.merged_for_whole_life);
}

#[test]
fn a_node_project_gets_the_npm_registry_and_adding_it_moves_the_digest() {
    let f = fixture();
    let before = f.plan();
    std::fs::write(f.project.path().join("web/package.json"), "{}").unwrap();
    let after = f.plan();
    assert!(after.network.build_hosts.contains(&"registry.npmjs.org".to_owned()));
    assert!(!after.network.build_hosts.contains(&"proxy.golang.org".to_owned()));
    // The hosts are part of what a person approves.
    assert_ne!(before.digest, after.digest);
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
            FakeCall::ExecStream(..) => "exec-stream",
            FakeCall::ExecWithInput(..) => "exec-with-input",
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

/// Ports are fixed at creation, so approving more later must not claim success.
#[test]
fn changing_the_ports_of_an_existing_sandbox_is_refused_and_not_reported_as_done() {
    let f = fixture();
    f.up(vec![]).unwrap();
    let wanted = f.plan().ports[0].host_port;
    let error = f.up(vec![wanted]).unwrap_err();
    assert_eq!(error.code, ErrorCode::WrongState);
    assert!(error.message.contains("down --destroy"), "{}", error.message);
    let audit = std::fs::read_to_string(f.service.audit().path()).unwrap();
    assert!(!audit.contains("port_forwarded"), "{audit}");
    // The same ports again is an ordinary restart.
    f.up(vec![]).unwrap();
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

fn blocked(f: &Fixture) -> Result<proto::BlockedReport, proto::ApiError> {
    match f.call(Request::Blocked(proto::BlockedArgs {
        project: f.path(),
        after: None,
    }))? {
        Reply::Blocked(report) => Ok(report),
        other => panic!("expected a blocked report, got {other:?}"),
    }
}

/// A project started on an engine that keeps real files, with the folder they
/// live in, and that sandbox's name.
fn recording_fixture() -> (Fixture, tempfile::TempDir, engine::SandboxName) {
    let f = fixture();
    let home = tempfile::tempdir().unwrap();
    f.deps.engine.set_location(EngineLocation {
        binary: std::path::PathBuf::from("/nonexistent"),
        home: home.path().join("msb"),
    });
    f.deps.engine.keep_files();
    f.up(Vec::new()).unwrap();
    let name = engine::SandboxName::new(sandbox_name_of(&f)).unwrap();
    (f, home, name)
}

#[test]
fn a_project_with_no_sandbox_is_not_recording_and_reads_nothing() {
    let f = fixture();
    let report = blocked(&f).unwrap();
    assert_eq!(report.recording, proto::BlockedRecording::NoSandbox);
    assert_eq!(report.trust, proto::LogTrust::NotRead);
    assert!(report.events.is_empty());
}

#[test]
fn a_new_sandbox_records_and_its_level_is_on_the_hosts_record() {
    let (f, _home, name) = recording_fixture();
    f.deps.engine.deny_lookup(&name, "a.example.net").unwrap();
    f.deps.engine.deny_lookup(&name, "A.example.net").unwrap();
    f.deps.engine.deny_lookup(&name, "b.example.net").unwrap();
    let report = blocked(&f).unwrap();
    assert_eq!(report.recording, proto::BlockedRecording::Recording);
    assert_eq!(report.log_level.as_deref(), Some("debug"));
    assert_eq!(report.trust, proto::LogTrust::HostOnly);
    assert_eq!(report.total, 3);
    let names: Vec<(&str, u64)> = report
        .events
        .iter()
        .map(|e| (e.domain.as_str(), e.count))
        .collect();
    assert_eq!(names, [("a.example.net", 2), ("b.example.net", 1)]);

    let Reply::Status(status) = f
        .call(Request::Status(ProjectArgs { project: f.path() }))
        .unwrap()
    else {
        panic!("expected a status")
    };
    assert_eq!(status.blocked_count, 3);
    assert_eq!(status.blocked.unwrap().domains, 2);
}

#[test]
fn a_stopped_sandbox_can_still_be_read_and_following_gets_only_what_is_new() {
    let (f, _home, name) = recording_fixture();
    f.deps.engine.deny_lookup(&name, "a.example.net").unwrap();
    let first = blocked(&f).unwrap();
    f.deps.engine.deny_lookup(&name, "b.example.net").unwrap();
    f.call(Request::Down(DownArgs {
        project: f.path(),
        destroy: false,
    }))
    .unwrap();
    let Reply::Blocked(second) = f
        .call(Request::Blocked(proto::BlockedArgs {
            project: f.path(),
            after: first.next,
        }))
        .unwrap()
    else {
        panic!("expected a blocked report")
    };
    assert_eq!(second.total, 1);
    assert_eq!(second.events[0].domain, "b.example.net");
    // Reading does not start anything.
    assert!(!f
        .deps
        .engine
        .calls()
        .iter()
        .rev()
        .take(2)
        .any(|c| matches!(c, FakeCall::Start(_) | FakeCall::Exec(..))));
}

#[test]
fn destroying_forgets_the_level_and_the_log() {
    let (f, _home, name) = recording_fixture();
    f.deps.engine.deny_lookup(&name, "a.example.net").unwrap();
    f.call(Request::Down(DownArgs {
        project: f.path(),
        destroy: true,
    }))
    .unwrap();
    let report = blocked(&f).unwrap();
    assert_eq!(report.recording, proto::BlockedRecording::NoSandbox);
    assert!(report.events.is_empty());
}

#[test]
fn a_link_in_place_of_the_logs_folder_is_refused_and_named() {
    let (f, home, name) = recording_fixture();
    let logs = f.deps.engine.logs_dir(&name).unwrap();
    std::fs::remove_dir_all(&logs).unwrap();
    std::os::unix::fs::symlink(home.path(), &logs).unwrap();
    let error = blocked(&f).unwrap_err();
    assert_eq!(error.code, ErrorCode::Refused);
    assert!(error.message.contains("symbolic link"), "{}", error.message);
}

/// An engine that cannot say where its files are has nothing to read, and that
/// is said rather than shown as an empty list.
#[test]
fn an_engine_with_no_files_says_so() {
    let f = fixture();
    f.up(Vec::new()).unwrap();
    let error = blocked(&f).unwrap_err();
    assert_eq!(error.code, ErrorCode::Unsupported);
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
        snapshot_adds: None,
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
        vec!["planning", "sandbox", "snapshot", "docker", "secrets", "build"]
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

// ---- logs and terminals

/// Keeps the output a request sends, in order.
#[derive(Default)]
struct Kept {
    output: Vec<(OutputStream, String)>,
}

impl Progress for Kept {
    fn step(&mut self, _step: &str, _detail: Option<&str>) {}
    fn output(&mut self, stream: OutputStream, text: &str) -> bool {
        self.output.push((stream, text.to_owned()));
        true
    }
}

impl Kept {
    fn text(&self, wanted: OutputStream) -> String {
        self.output
            .iter()
            .filter(|(stream, _)| *stream == wanted)
            .map(|(_, text)| text.as_str())
            .collect()
    }
}

impl Fixture {
    fn logs_args(&self, service: Option<&str>, tail: u32, follow: bool) -> Request {
        Request::Logs(LogsArgs {
            project: self.path(),
            service: service.map(str::to_owned),
            tail,
            follow,
        })
    }

    fn terminal_args(&self, service: Option<&str>) -> Request {
        Request::Terminal(TerminalArgs {
            project: self.path(),
            service: service.map(str::to_owned),
        })
    }

    fn started(self) -> Self {
        self.up(Vec::new()).unwrap();
        self
    }

    fn stopped(self) -> Self {
        let started = self.started();
        started
            .call(Request::Down(DownArgs {
                project: started.path(),
                destroy: false,
            }))
            .unwrap();
        started
    }

    fn logs(&self, service: Option<&str>) -> Result<(proto::LogsReport, Kept), proto::ApiError> {
        let mut kept = Kept::default();
        let reply = self
            .service
            .handle(&self.logs_args(service, 200, false), &mut kept)?;
        match reply {
            Reply::Logs(report) => Ok((report, kept)),
            other => panic!("unexpected {other:?}"),
        }
    }
}

/// The engine's `exec` boots a stopped sandbox, so the only call allowed on one
/// that is not running is asking what state it is in.
fn assert_only_status(calls: &[FakeCall]) {
    assert!(
        calls.iter().all(|call| matches!(call, FakeCall::Status(_))),
        "{calls:?}"
    );
}

#[test]
fn logs_run_compose_in_the_project_folder() {
    let f = fixture().started();
    f.logs(Some("web")).unwrap();
    let request = f
        .deps
        .engine
        .calls()
        .into_iter()
        .find_map(|call| match call {
            FakeCall::ExecStream(_, request) => Some(request),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        request.command,
        ["docker", "compose", "logs", "--no-color", "--tail", "200", "web"]
    );
    assert_eq!(request.workdir.unwrap().as_str(), "/work");
    assert!(request.timeout.is_some());
}

#[test]
fn log_output_is_filtered_here_even_when_a_sequence_is_split_across_chunks() {
    let f = fixture().started();
    for chunk in [
        &b"a\x1b[3"[..],
        b"1mb\x1b]0;evil\x07c\xe2\x82",
        b"\xac\x07\n",
    ] {
        f.deps.engine.push_stream_output(StreamKind::Stdout, chunk);
    }
    f.deps
        .engine
        .push_stream_output(StreamKind::Stderr, b"oops\x1b[2J\n");
    f.deps.engine.set_stream_code(4);
    let (report, kept) = f.logs(None).unwrap();
    assert_eq!(kept.text(OutputStream::Stdout), "abc\u{20ac}\\u{7}\n");
    assert_eq!(kept.text(OutputStream::Stderr), "oops\n");
    assert_eq!(report.code, Some(4));
    assert!(!report.truncated);
}

#[test]
fn an_unfinished_character_at_the_end_of_the_output_is_still_sent_safely() {
    let f = fixture().started();
    f.deps
        .engine
        .push_stream_output(StreamKind::Stdout, b"end\xe2\x82");
    let (_, kept) = f.logs(None).unwrap();
    assert_eq!(kept.text(OutputStream::Stdout), "end\u{fffd}");
}

/// Each byte here grows to several in the output, and a frame over the limit
/// would be refused, so the largest chunk the engine sends must still fit.
#[test]
fn output_that_grows_when_escaped_still_fits_in_a_frame() {
    let f = fixture().started();
    let chunk = vec![0x07u8; 80 * 1024];
    f.deps.engine.push_stream_output(StreamKind::Stdout, &chunk);
    let (_, kept) = f.logs(None).unwrap();
    for (stream, text) in kept.output {
        let frame = proto::Frame::Output { stream, text };
        let mut buffer = Vec::new();
        proto::write_frame(&mut buffer, &frame).unwrap();
    }
}

#[test]
fn a_plain_read_stops_at_its_size_limit_and_says_so() {
    let f = fixture().started();
    for _ in 0..70 {
        f.deps
            .engine
            .push_stream_output(StreamKind::Stdout, &vec![b'x'; 1024 * 1024]);
    }
    let (report, kept) = f.logs(None).unwrap();
    assert!(report.truncated);
    assert_eq!(report.code, None);
    assert!(kept.text(OutputStream::Stdout).len() <= 64 * 1024 * 1024);
}

#[test]
fn a_bad_service_name_is_refused_before_the_engine_is_asked_anything() {
    let f = fixture().started();
    let earlier = f.deps.engine.calls().len();
    for name in ["-f", "--follow", "a;b", "a b", "", "$(id)", "a/b", "a\nb"] {
        for request in [
            f.logs_args(Some(name), 10, false),
            f.logs_args(Some(name), 10, true),
            f.terminal_args(Some(name)),
        ] {
            let error = f.call(request).unwrap_err();
            assert_eq!(error.code, ErrorCode::BadRequest, "{name:?}");
        }
    }
    assert_eq!(f.deps.engine.calls().len(), earlier);
}

#[test]
fn asking_for_too_many_lines_is_refused() {
    let f = fixture().started();
    let error = f
        .call(f.logs_args(None, proto::MAX_LOG_TAIL + 1, false))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::BadRequest);
}

#[test]
fn logs_and_terminals_refuse_a_stopped_sandbox_without_booting_it() {
    let f = fixture().stopped();
    f.deps.engine.set_location(EngineLocation {
        binary: "/engine/bin".into(),
        home: "/engine/home".into(),
    });
    let earlier = f.deps.engine.calls().len();
    for request in [
        f.logs_args(None, 10, false),
        f.logs_args(Some("web"), 10, true),
        f.terminal_args(None),
        f.terminal_args(Some("web")),
    ] {
        let error = f.call(request).unwrap_err();
        assert_eq!(error.code, ErrorCode::WrongState);
        assert!(error.message.contains("stopped"), "{}", error.message);
        assert!(error.message.contains(" up "), "{}", error.message);
    }
    assert_only_status(&f.deps.engine.calls()[earlier..]);
    assert_eq!(
        f.deps.engine.status(&sandbox_of(&f)).unwrap(),
        engine::SandboxStatus::Stopped
    );
}

fn sandbox_of(f: &Fixture) -> engine::SandboxName {
    f.deps
        .engine
        .calls()
        .into_iter()
        .find_map(|call| match call {
            FakeCall::Create(spec) => Some(spec.name),
            _ => None,
        })
        .unwrap()
}

#[test]
fn logs_and_terminals_refuse_a_project_with_no_sandbox() {
    let f = fixture();
    for request in [f.logs_args(None, 10, false), f.terminal_args(None)] {
        let error = f.call(request).unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound);
        assert!(error.message.contains(" up "), "{}", error.message);
    }
    assert_only_status(&f.deps.engine.calls());
}

#[test]
fn a_terminal_gets_the_sandbox_and_the_engine_and_nothing_runs_for_it() {
    let f = fixture().started();
    f.deps.engine.set_location(EngineLocation {
        binary: "/engine/bin".into(),
        home: "/engine/home".into(),
    });
    let earlier = f.deps.engine.calls().len();
    let Reply::Terminal(report) = f.call(f.terminal_args(Some("web"))).unwrap() else {
        panic!("unexpected reply");
    };
    assert_eq!(report.sandbox, sandbox_of(&f).as_str());
    assert_eq!(report.engine_binary, "/engine/bin");
    assert_eq!(report.engine_home, "/engine/home");
    assert_eq!(report.workdir, "/work");
    assert_only_status(&f.deps.engine.calls()[earlier..]);

    let audit = std::fs::read_to_string(f.service.audit().path()).unwrap();
    assert!(audit.contains("terminal_opened"), "{audit}");
    assert!(audit.contains("\"subject\":\"web\""), "{audit}");
}

#[test]
fn an_engine_with_nowhere_to_run_from_is_refused() {
    let f = fixture().started();
    let error = f.call(f.terminal_args(None)).unwrap_err();
    assert_eq!(error.code, ErrorCode::Unsupported);
}

#[test]
fn an_engine_path_that_could_not_arrive_exactly_is_refused() {
    let f = fixture().started();
    f.deps.engine.set_location(EngineLocation {
        binary: "/engine\u{1b}[0m/bin".into(),
        home: "/engine/home".into(),
    });
    let error = f.call(f.terminal_args(None)).unwrap_err();
    assert_eq!(error.code, ErrorCode::Internal);
}

/// A client on a real socket: what Ctrl-C on a followed log looks like from
/// here is a connection closing while nothing is being written.
#[test]
fn a_followed_log_stops_when_the_client_disconnects_and_the_sandbox_keeps_running() {
    let dir = tempfile::Builder::new().prefix("d").tempdir_in("/tmp").unwrap();
    let deps = Arc::new(TestDeps::new());
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("compose.yaml"), COMPOSE).unwrap();
    std::fs::create_dir_all(project.path().join("web")).unwrap();
    std::fs::write(project.path().join("web/Dockerfile"), "FROM scratch\n").unwrap();
    std::fs::write(
        project.path().join(brand::CONFIG_FILE),
        "resources:\n  memory: 4G\n  cpus: 2\n  disk: 30G\n",
    )
    .unwrap();
    let state = dir.path().join(brand::STATE_DIR);
    let service = Arc::new(
        Service::new(
            StatePaths {
                state: state.clone(),
                overridden: true,
            },
            Arc::clone(&deps) as Arc<dyn Deps>,
        )
        .unwrap(),
    );
    let root = project.path().to_string_lossy().into_owned();
    let Reply::Plan(plan) = service
        .handle(
            &Request::Plan(PlanArgs {
                project: root.clone(),
                source: Source::default(),
            }),
            &mut Quiet,
        )
        .unwrap()
    else {
        panic!("unexpected reply");
    };
    service
        .handle(
            &Request::Up(UpArgs {
                project: root.clone(),
                source: Source::default(),
                plan_digest: plan.digest,
                approve_ports: Vec::new(),
            }),
            &mut Quiet,
        )
        .unwrap();

    let listener = daemon::server::bind(&state).unwrap();
    let serving = Arc::clone(&service);
    std::thread::spawn(move || daemon::server::serve(serving, &listener));

    deps.engine
        .push_stream_output(StreamKind::Stdout, b"first line\n");
    deps.engine.keep_streams_open();

    let stream = std::os::unix::net::UnixStream::connect(daemon::server::socket_path(&state)).unwrap();
    let mut writer = stream.try_clone().unwrap();
    proto::write_frame(
        &mut writer,
        &proto::Envelope::new(Request::Logs(LogsArgs {
            project: root,
            service: None,
            tail: 10,
            follow: true,
        })),
    )
    .unwrap();
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let frame: proto::Frame = proto::read_frame(&mut reader).unwrap().unwrap();
    assert_eq!(
        frame,
        proto::Frame::Output {
            stream: OutputStream::Stdout,
            text: "first line\n".to_owned()
        }
    );
    assert_eq!(deps.engine.streams_stopped(), 0, "it should still be following");

    drop(reader);
    drop(writer);
    drop(stream);

    let started = std::time::Instant::now();
    while deps.engine.streams_stopped() == 0 {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the followed log was not stopped"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let name = deps
        .engine
        .calls()
        .into_iter()
        .find_map(|call| match call {
            FakeCall::Create(spec) => Some(spec.name),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        deps.engine.status(&name).unwrap(),
        engine::SandboxStatus::Running
    );
    assert!(!deps
        .engine
        .calls()
        .iter()
        .any(|call| matches!(call, FakeCall::Stop(_) | FakeCall::Destroy(_))));
}

// --- Secrets -----------------------------------------------------------
//
// The value used throughout is fake. What these check is where it goes and,
// more importantly, where it does not.

const FAKE_VALUE: &str = "sk-fake-do-not-use-9f2c";

/// A store that keeps what is put in it, so a secret survives from the test
/// into the daemon. `TestDeps::store` hands out a fresh one each call, which
/// is right for the tests that want nothing stored.
#[derive(Default)]
struct Shared(std::sync::Arc<MemoryStore>);

impl Clone for Shared {
    fn clone(&self) -> Self {
        Self(std::sync::Arc::clone(&self.0))
    }
}

impl SecretStore for Shared {
    fn put(
        &self,
        name: &secrets::SecretName,
        value: &secrets::SecretValue,
    ) -> Result<(), secrets::StoreError> {
        self.0.put(name, value)
    }
    fn get(
        &self,
        name: &secrets::SecretName,
    ) -> Result<Option<secrets::SecretValue>, secrets::StoreError> {
        self.0.get(name)
    }
    fn delete(&self, name: &secrets::SecretName) -> Result<bool, secrets::StoreError> {
        self.0.delete(name)
    }
}

struct SecretDeps {
    inner: TestDeps,
    store: Shared,
}

impl Deps for SecretDeps {
    fn engine(&self) -> &dyn Engine {
        self.inner.engine()
    }
    fn now_unix(&self) -> u64 {
        self.inner.now_unix()
    }
    fn host_info(&self, disk_path: &Path) -> HostInfo {
        self.inner.host_info(disk_path)
    }
    fn store(&self, _project: &ProjectId) -> Box<dyn SecretStore> {
        Box::new(self.store.clone())
    }
    fn snapshot(&self, root: &Path, source: &Source, dest: &Path) -> Result<SnapshotPlan, String> {
        self.inner.snapshot(root, source, dest)
    }
}

struct SecretFixture {
    _state: tempfile::TempDir,
    project: tempfile::TempDir,
    service: Service,
    deps: Arc<SecretDeps>,
}

impl SecretFixture {
    fn path(&self) -> String {
        self.project.path().to_string_lossy().into_owned()
    }

    fn plan(&self) -> proto::PlanReport {
        match self
            .service
            .handle(
                &Request::Plan(PlanArgs {
                    project: self.path(),
                    source: Source::default(),
                }),
                &mut Quiet,
            )
            .unwrap()
        {
            Reply::Plan(plan) => *plan,
            other => panic!("unexpected {other:?}"),
        }
    }

    fn up(&self) -> Result<proto::UpReport, proto::ApiError> {
        let Reply::Plan(plan) = self
            .service
            .handle(
                &Request::Plan(PlanArgs {
                    project: self.path(),
                    source: Source::default(),
                }),
                &mut Quiet,
            )?
        else {
            panic!("expected a plan")
        };
        match self.service.handle(
            &Request::Up(UpArgs {
                project: self.path(),
                source: Source::default(),
                plan_digest: plan.digest,
                approve_ports: vec![],
            }),
            &mut Quiet,
        )? {
            Reply::Up(report) => Ok(report),
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Everything the engine was asked to do, as text. The fake deliberately
    /// leaves standard input out of this.
    fn calls_as_text(&self) -> String {
        format!("{:?}", self.deps.inner.engine.calls())
    }
}

fn with_secret(delivery: secrets::Delivery, services: &[&str]) -> SecretFixture {
    with_secret_valued(delivery, services, FAKE_VALUE)
}

fn with_secret_valued(
    delivery: secrets::Delivery,
    services: &[&str],
    value: &str,
) -> SecretFixture {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("compose.yaml"), COMPOSE).unwrap();
    std::fs::create_dir_all(project.path().join("web")).unwrap();
    std::fs::write(project.path().join("web/Dockerfile"), "FROM scratch\n").unwrap();
    std::fs::write(
        project.path().join(brand::CONFIG_FILE),
        "resources:\n  memory: 4G\n  cpus: 2\n  disk: 30G\n",
    )
    .unwrap();
    with_secret_in(project, delivery, services, value)
}

/// Any project, with `ENCRYPTION_KEY` assigned to the named services.
fn with_secret_in(
    project: tempfile::TempDir,
    delivery: secrets::Delivery,
    services: &[&str],
    value: &str,
) -> SecretFixture {
    let state = tempfile::tempdir().unwrap();
    let store = Shared::default();
    let name = secrets::SecretName::new("ENCRYPTION_KEY").unwrap();
    store
        .put(&name, &secrets::SecretValue::new(value))
        .unwrap();

    let paths = StatePaths {
        state: state.path().join(brand::STATE_DIR),
        overridden: true,
    };
    let canonical = std::fs::canonicalize(project.path()).unwrap();
    let id = hoststate::project_id(&canonical).unwrap();
    let dir = hoststate::prepare_project(&paths, &id).unwrap();
    let mut meta = hoststate::ProjectMeta::new(&canonical);
    meta.assignments = secrets::AssignmentSet::new(vec![secrets::Assignment::new(
        name,
        delivery,
        services
            .iter()
            .map(|s| secrets::ServiceName::new(*s).unwrap())
            .collect(),
    )
    .unwrap()])
    .unwrap();
    hoststate::write_meta(&dir, &meta).unwrap();

    let deps = Arc::new(SecretDeps {
        inner: TestDeps::new(),
        store,
    });
    let service = Service::new(paths, Arc::clone(&deps) as Arc<dyn Deps>).unwrap();
    SecretFixture {
        _state: state,
        project,
        service,
        deps,
    }
}

/// The one that matters: a value handed to the engine as an argument is
/// readable by anyone on the host, from the process list.
#[test]
fn a_secret_reaches_the_sandbox_over_standard_input_and_never_in_an_argument() {
    let f = with_secret(secrets::Delivery::Injected, &["api"]);
    let report = f.up().unwrap();

    let written: Vec<(Vec<String>, String)> = f.deps.inner.engine.inputs();
    assert!(
        written.iter().any(|(_, body)| body.contains(FAKE_VALUE)),
        "the value never reached the sandbox"
    );
    assert!(
        !f.calls_as_text().contains(FAKE_VALUE),
        "the value appeared in an argument list"
    );
    assert_eq!(report.secrets.len(), 1);
    assert_eq!(report.secrets[0].service, "api");
    assert_eq!(report.secrets[0].names, vec!["ENCRYPTION_KEY"]);
}

#[test]
fn an_environment_secret_is_written_as_one_line_for_the_named_service_only() {
    let f = with_secret(secrets::Delivery::Injected, &["api"]);
    f.up().unwrap();
    let written = f.deps.inner.engine.inputs();
    let env = written
        .iter()
        .find(|(_, body)| body.contains("ENCRYPTION_KEY="))
        .expect("no environment file was written");
    assert_eq!(env.1, format!("ENCRYPTION_KEY={FAKE_VALUE}\n"));

    let yaml = &written
        .iter()
        .find(|(_, body)| body.contains("services:"))
        .expect("no override was written")
        .1;
    assert!(yaml.contains("\"api\""), "{yaml}");
    assert!(!yaml.contains("\"web\""), "{yaml}");
    // The override carries paths, never a value.
    assert!(!yaml.contains(FAKE_VALUE), "{yaml}");
}

#[test]
fn a_file_secret_is_declared_rather_than_put_in_the_environment() {
    let f = with_secret(secrets::Delivery::InjectedFile, &["api"]);
    f.up().unwrap();
    let written = f.deps.inner.engine.inputs();
    assert!(
        written
            .iter()
            .any(|(_, body)| body.as_str() == FAKE_VALUE),
        "the value was not written as a file"
    );
    assert!(
        !written.iter().any(|(_, b)| b.contains("ENCRYPTION_KEY=")),
        "a file secret must not also go in the environment"
    );
    let yaml = &written
        .iter()
        .find(|(_, body)| body.contains("services:"))
        .unwrap()
        .1;
    assert!(yaml.contains("secrets:"), "{yaml}");
    assert!(!yaml.contains("env_file"), "{yaml}");
}

#[test]
fn nothing_is_written_under_the_project_directory() {
    let f = with_secret(secrets::Delivery::Injected, &["api"]);
    f.up().unwrap();
    for (command, _) in f.deps.inner.engine.inputs() {
        let target = command.last().expect("a path argument");
        assert!(target.starts_with("/run/"), "{target}");
        assert!(!target.contains("/work"), "{target}");
    }
}

#[test]
fn the_compose_command_names_the_override_only_when_there_is_one() {
    let with = with_secret(secrets::Delivery::Injected, &["api"]);
    with.up().unwrap();
    assert!(
        with.calls_as_text().contains("override.yaml"),
        "the override was not passed to compose"
    );

    let without = fixture();
    without.up(vec![]).unwrap();
    let text = format!("{:?}", without.deps.engine.calls());
    assert!(!text.contains("override.yaml"), "{text}");
}

#[test]
fn a_secret_with_a_line_break_is_refused_rather_than_escaped_by_guesswork() {
    let f = with_secret_valued(
        secrets::Delivery::Injected,
        &["api"],
        "real\nINJECTED_BY_THE_VALUE=1",
    );
    let error = f.up().unwrap_err();
    assert_eq!(error.code, ErrorCode::Refused);
    assert!(error.message.contains("--as-file"), "{}", error.message);
    assert!(!error.message.contains("INJECTED_BY_THE_VALUE"));
}

#[test]
fn a_proxy_secret_is_refused_while_the_question_is_open() {
    let f = with_secret(
        secrets::Delivery::Proxy {
            bound_host: secrets::BoundHost::new("api.example.com").unwrap(),
        },
        &["api"],
    );
    let error = f.up().unwrap_err();
    assert_eq!(error.code, ErrorCode::Refused);
}

/// Carrying on without the temporary filesystem would put the value on the
/// sandbox's own disk, which is the quiet failure this must not have.
#[test]
fn delivery_stops_when_there_is_nowhere_safe_to_put_a_secret() {
    let f = with_secret(secrets::Delivery::Injected, &["api"]);
    // The first exec of the start is the Docker readiness check; the mount
    // attempt is the one that matters, so fail every exec after readiness.
    f.deps.inner.engine.push_exec_result(engine::ExecOutput {
        code: 0,
        ..Default::default()
    });
    f.deps.inner.engine.push_exec_result(engine::ExecOutput {
        code: 1,
        stderr: "mount: permission denied".to_owned(),
        ..Default::default()
    });
    let error = f.up().unwrap_err();
    assert_eq!(error.code, ErrorCode::Refused);
    assert!(
        error.message.contains("nowhere safe"),
        "{}",
        error.message
    );
    assert!(
        f.deps.inner.engine.inputs().is_empty(),
        "a value was written even though there was nowhere safe for it"
    );
}

#[test]
fn the_host_records_which_secret_went_where_by_name_only() {
    let f = with_secret(secrets::Delivery::Injected, &["api"]);
    f.up().unwrap();
    let text = std::fs::read_to_string(f.service.audit().path()).unwrap();
    assert!(text.contains("secret_delivered"), "{text}");
    assert!(text.contains("ENCRYPTION_KEY"), "{text}");
    assert!(!text.contains(FAKE_VALUE), "a value reached the audit log");
}

#[test]
fn a_plan_names_a_secret_and_its_route_but_never_its_value() {
    let f = with_secret(secrets::Delivery::Injected, &["api"]);
    let Reply::Plan(plan) = f
        .service
        .handle(
            &Request::Plan(PlanArgs {
                project: f.path(),
                source: Source::default(),
            }),
            &mut Quiet,
        )
        .unwrap()
    else {
        panic!("expected a plan")
    };
    assert_eq!(plan.secrets.len(), 1);
    assert_eq!(plan.secrets[0].name, "ENCRYPTION_KEY");
    assert_eq!(plan.secrets[0].route, proto::SecretRoute::Injected);
    assert_eq!(plan.secrets[0].services, vec!["api"]);
    assert!(!format!("{plan:?}").contains(FAKE_VALUE));
}

// A project with only a Dockerfile.

/// A copy of `tests/fixtures/dockerfile-only`, so a test can change it.
fn dockerfile_project() -> tempfile::TempDir {
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/dockerfile-only");
    let project = tempfile::tempdir().unwrap();
    for entry in std::fs::read_dir(source).unwrap().flatten() {
        std::fs::copy(entry.path(), project.path().join(entry.file_name())).unwrap();
    }
    project
}

fn dockerfile_fixture() -> Fixture {
    fixture_from(dockerfile_project(), TestDeps::new())
}

fn create_spec(f: &Fixture) -> engine::CreateSpec {
    f.deps
        .engine
        .calls()
        .into_iter()
        .find_map(|call| match call {
            FakeCall::Create(spec) => Some(spec),
            _ => None,
        })
        .expect("a sandbox was created")
}

#[test]
fn a_dockerfile_project_plans_as_one_generated_service() {
    let plan = dockerfile_fixture().plan();
    assert_eq!(plan.blockers, Vec::<String>::new());
    assert_eq!(plan.kind, "dockerfile");
    assert_eq!(plan.services, vec!["app"]);
    assert_eq!(plan.compose_files, vec!["compose.yaml"]);
    // Only the last stage's TCP ports, and only plain numbers.
    let ports: Vec<(u16, u16)> = plan
        .ports
        .iter()
        .map(|p| (p.host_port, p.container_port))
        .collect();
    assert_eq!(ports, vec![(3000, 3000), (8080, 8080)]);
    assert!(plan
        .ports
        .iter()
        .all(|p| p.service == "app" && !p.was_all_interfaces));
    // It builds, so it gets the build memory floor like any other build.
    assert!(plan.resources.build_memory_bytes >= daemon::plan::MIN_BUILD_MEMORY_BYTES);

    // Each odd EXPOSE entry is reported.
    assert_eq!(plan.findings.len(), 6, "{:#?}", plan.findings);
    assert!(plan.findings.iter().all(|f| f.severity == "Info"));
    let all = format!("{:?}", plan.findings);
    for word in ["UDP", "variable", "range", "not a port number"] {
        assert!(all.contains(word), "{word} in {all}");
    }

    let generated = plan.generated_compose.as_deref().unwrap();
    assert!(generated.contains("dockerfile: Dockerfile"), "{generated}");
    for absent in [
        "9229", "2222", "5353", "9230", "4444", "touch", "rm", "volumes", "privileged",
    ] {
        assert!(!generated.contains(absent), "{absent} in {generated}");
    }
}

#[test]
fn a_dockerfile_project_starts_and_its_ports_need_approval() {
    let f = dockerfile_fixture();
    let report = f.up(vec![]).unwrap();
    assert_eq!(report.sandbox, SandboxState::Running);
    assert!(report.ports.is_empty());
    // Offered is not forwarded.
    assert!(create_spec(&f).ports.is_empty());

    let f = dockerfile_fixture();
    let report = f.up(vec![8080]).unwrap();
    assert_eq!(report.ports, vec![8080]);
    assert_eq!(create_spec(&f).ports, vec![engine::HostPort(8080)]);
}

#[test]
fn a_port_from_an_earlier_stage_or_a_hostile_line_cannot_be_approved() {
    for port in [9229, 2222, 5353, 4444, 80] {
        let f = dockerfile_fixture();
        let error = f.up(vec![port]).unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest, "{port}");
        assert!(!f
            .deps
            .engine
            .calls()
            .iter()
            .any(|c| matches!(c, FakeCall::Create(_))));
    }
}

#[test]
fn the_generated_compose_file_reaches_the_engine_and_is_what_runs() {
    let f = dockerfile_fixture();
    let plan = f.plan();
    f.up(vec![3000]).unwrap();
    let sandbox = sandbox_of(&f);

    let copied = f
        .deps
        .engine
        .copied_file(&sandbox, "/work/compose.yaml")
        .expect("the generated file was copied in");
    assert_eq!(
        String::from_utf8(copied).unwrap(),
        plan.generated_compose.unwrap()
    );
    // The project's own files went with it, as the build context.
    assert!(f
        .deps
        .engine
        .copied_file(&sandbox, "/work/Dockerfile")
        .is_some());

    let up = f
        .deps
        .engine
        .calls()
        .into_iter()
        .find_map(|call| match call {
            FakeCall::Exec(_, request) if request.command.contains(&"up".to_owned()) => {
                Some(request)
            }
            _ => None,
        })
        .expect("compose up ran");
    assert_eq!(&up.command[..4], ["docker", "compose", "-f", "compose.yaml"]);
    assert_eq!(up.workdir.unwrap().as_str(), "/work");
}

#[test]
fn a_changed_expose_line_changes_the_digest() {
    let f = dockerfile_fixture();
    let before = f.plan();
    let dockerfile = f.project.path().join("Dockerfile");
    let text = std::fs::read_to_string(&dockerfile).unwrap();
    std::fs::write(&dockerfile, text.replace("EXPOSE 3000", "EXPOSE 3001")).unwrap();
    let after = f.plan();
    assert_ne!(before.digest, after.digest);
    assert_ne!(before.generated_compose, after.generated_compose);

    // The earlier approval no longer starts anything.
    let error = f
        .call(Request::Up(UpArgs {
            project: f.path(),
            source: Source::default(),
            plan_digest: before.digest,
            approve_ports: vec![],
        }))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Refused);
}

#[test]
fn a_compose_file_wins_over_a_dockerfile() {
    let project = dockerfile_project();
    std::fs::write(
        project.path().join("compose.yaml"),
        "services:\n  web:\n    image: web:1\n",
    )
    .unwrap();
    let plan = fixture_from(project, TestDeps::new()).plan();
    assert_eq!(plan.kind, "compose");
    assert_eq!(plan.services, vec!["web"]);
    assert_eq!(plan.generated_compose, None);
}

#[test]
fn an_override_file_with_no_compose_file_blocks_a_dockerfile_project() {
    let project = dockerfile_project();
    std::fs::write(
        project.path().join("compose.override.yaml"),
        "services:\n  app:\n    privileged: true\n",
    )
    .unwrap();
    let plan = fixture_from(project, TestDeps::new()).plan();
    assert_eq!(plan.kind, "dockerfile");
    assert!(
        plan.blockers
            .iter()
            .any(|b| b.contains("compose.override.yaml")),
        "{:?}",
        plan.blockers
    );
}

#[test]
fn a_snapshot_that_has_its_own_compose_file_is_not_overwritten() {
    let deps = TestDeps {
        snapshot_adds: Some(("compose.yaml", "services:\n  app:\n    privileged: true\n")),
        ..TestDeps::new()
    };
    let f = fixture_from(dockerfile_project(), deps);
    let error = f.up(vec![]).unwrap_err();
    assert_eq!(error.code, ErrorCode::Refused);
    assert!(error.message.contains("compose.yaml"), "{}", error.message);
    // Nothing was copied in, and the empty sandbox was removed.
    let calls = f.deps.engine.calls();
    assert!(!calls.iter().any(|c| matches!(c, FakeCall::CopyIn(..))));
    assert!(calls.iter().any(|c| matches!(c, FakeCall::Destroy(..))));
}

#[test]
fn other_kinds_of_project_are_still_refused() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("package.json"), "{}").unwrap();
    let f = fixture_from(project, TestDeps::new());
    let error = f
        .call(Request::Plan(PlanArgs {
            project: f.path(),
            source: Source::default(),
        }))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Unsupported);
    assert!(
        error
            .message
            .contains("only Compose projects and projects with a Dockerfile can be started"),
        "{}",
        error.message
    );
}

// The hostile demo: one service holds a secret, another tries to take it.

/// A copy of a folder under `tests/fixtures`, subfolders included.
fn copy_fixture(name: &str) -> tempfile::TempDir {
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap().flatten() {
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }
    let project = tempfile::tempdir().unwrap();
    copy(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name),
        project.path(),
    );
    project
}

#[test]
fn a_service_that_could_reach_another_services_secret_stops_the_start() {
    let f = with_secret_in(
        copy_fixture("hostile-demo"),
        secrets::Delivery::Injected,
        &["api"],
        FAKE_VALUE,
    );
    let plan = f.plan();
    let blockers = plan.blockers.join("\n");
    for expected in ["privileged", "pid", "/run", "docker.sock"] {
        assert!(
            plan.blockers
                .iter()
                .any(|b| b.contains("service evil") && b.contains(expected)),
            "no blocker mentions {expected}:\n{blockers}"
        );
    }
    // The service that holds the secret is not the one blamed.
    assert!(!blockers.contains("service api"), "{blockers}");

    let refused = f.up().expect_err("a blocked plan must not start");
    assert!(!format!("{refused:?}").contains(FAKE_VALUE));
    assert!(
        f.deps.inner.engine.inputs().is_empty(),
        "nothing may be delivered for a plan that was refused"
    );
}

#[test]
fn the_same_settings_without_secrets_are_findings_and_not_blockers() {
    let f = fixture_from(copy_fixture("hostile-demo"), TestDeps::new());
    let plan = f.plan();
    assert!(
        !plan.blockers.iter().any(|b| b.contains("could reach past")),
        "{:?}",
        plan.blockers
    );
    assert!(plan.findings.iter().any(|x| x.service.as_deref() == Some("evil")));
}

#[test]
fn the_runnable_demo_starts_and_its_secret_goes_only_to_api_over_standard_input() {
    let f = with_secret_in(
        copy_fixture("hostile-demo-run"),
        secrets::Delivery::Injected,
        &["api"],
        FAKE_VALUE,
    );
    assert_eq!(f.plan().blockers, Vec::<String>::new());
    let report = f.up().unwrap();
    assert_eq!(report.secrets.len(), 1);
    assert_eq!(report.secrets[0].service, "api");
    assert!(!f.calls_as_text().contains(FAKE_VALUE));
    // Nothing allowed while it runs unless a person approved it on the host.
    let plan = f.plan();
    assert!(plan.network.run_hosts.is_empty(), "{:?}", plan.network.run_hosts);
}

#[test]
fn a_dockerfile_projects_secret_reaches_its_one_service() {
    let f = with_secret_in(
        dockerfile_project(),
        secrets::Delivery::Injected,
        &["app"],
        FAKE_VALUE,
    );
    assert_eq!(f.plan().blockers, Vec::<String>::new());
    f.up().unwrap();
    let written = f.deps.inner.engine.inputs();
    let env = written
        .iter()
        .find(|(_, body)| body.contains("ENCRYPTION_KEY="))
        .expect("no environment file was written");
    assert_eq!(env.1, format!("ENCRYPTION_KEY={FAKE_VALUE}\n"));
    let yaml = &written
        .iter()
        .find(|(_, body)| body.contains("services:") && body.contains("env_file"))
        .expect("no override was written")
        .1;
    assert!(yaml.contains("\"app\""), "{yaml}");
    // The override is merged with the generated file, which Compose finds
    // in the work folder.
    let calls = f.calls_as_text();
    assert!(calls.contains("compose.yaml"), "{calls}");
    assert!(calls.contains("override.yaml"), "{calls}");
    assert!(!calls.contains(FAKE_VALUE));
}

// Masking secret values in what comes back from the sandbox.

#[test]
fn a_secret_printed_by_a_service_is_masked_in_logs_even_split_or_encoded() {
    let f = with_secret(secrets::Delivery::Injected, &["api"]);
    f.up().unwrap();
    let engine = &f.deps.inner.engine;
    let (first, second) = FAKE_VALUE.split_at(7);
    // Split between two reads, as a follow can deliver it.
    engine.push_stream_output(engine::OutputStream::Stdout, format!("api  | token={first}").as_bytes());
    engine.push_stream_output(engine::OutputStream::Stdout, format!("{second} ok\n").as_bytes());
    // Base64 of NAME=value, as the demo's attacker sent it.
    let encoded = {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let bytes = format!("ENCRYPTION_KEY={FAKE_VALUE}\n").into_bytes();
        let mut out = String::new();
        for c in bytes.chunks(3) {
            let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
            for i in 0..=c.len() {
                out.push(A[(n >> (18 - 6 * i)) as usize & 63] as char);
            }
        }
        out
    };
    engine.push_stream_output(engine::OutputStream::Stderr, format!("posted {encoded}\n").as_bytes());

    let mut kept = Kept::default();
    let request = Request::Logs(LogsArgs {
        project: f.path(),
        service: Some("api".to_owned()),
        tail: 200,
        follow: false,
    });
    f.service.handle(&request, &mut kept).unwrap();
    let all: String = kept.output.iter().map(|(_, t)| t.as_str()).collect();
    assert!(!all.contains(FAKE_VALUE), "{all}");
    assert!(!all.contains(first) || !all.contains(second), "{all}");
    assert!(all.contains("token=[ENCRYPTION_KEY] ok"), "{all}");
    assert!(all.contains("posted ") && all.matches("[ENCRYPTION_KEY]").count() == 2, "{all}");
}

#[test]
fn logs_for_a_project_with_no_secrets_are_passed_through_unchanged() {
    let f = fixture().started();
    f.deps
        .engine
        .push_stream_output(engine::OutputStream::Stdout, b"web  | ready\n");
    let (_, kept) = f.logs(Some("web")).unwrap();
    assert_eq!(kept.text(OutputStream::Stdout), "web  | ready\n");
}
