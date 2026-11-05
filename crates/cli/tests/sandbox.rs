//! `up`, `down` and `status`, driven through the command line into the real
//! daemon logic, with a fake engine underneath.
//!
//! Nothing here starts a VM. What it shows is that the command line asks the
//! daemon rather than deciding anything itself, that it will not forward a
//! port nobody approved, and that it says plainly what it does not know.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cli::{CallError, DaemonClient};
use common::Harness;
use engine::{
    Engine, EngineLocation, ExecRequest, FakeCall, FakeEngine, OutputStream as StreamKind,
};
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

    fn stream(
        &self,
        request: Request,
        output: &mut dyn FnMut(proto::OutputStream, &str) -> bool,
        _limit: Option<std::time::Duration>,
    ) -> Result<Reply, CallError> {
        struct Relay<'a> {
            output: &'a mut dyn FnMut(proto::OutputStream, &str) -> bool,
            stopped: bool,
        }
        impl daemon::Progress for Relay<'_> {
            fn step(&mut self, _step: &str, _detail: Option<&str>) {}
            fn output(&mut self, stream: proto::OutputStream, text: &str) -> bool {
                let more = (self.output)(stream, text);
                self.stopped |= !more;
                more
            }
        }
        let mut relay = Relay {
            output,
            stopped: false,
        };
        let result = self
            .service
            .handle(&request, &mut relay)
            .map_err(CallError::Refused);
        if relay.stopped {
            return Err(CallError::Stopped);
        }
        result
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
    harness_and_facts().0
}

fn harness_and_facts() -> (Harness, Arc<Facts>) {
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
        Arc::clone(&facts) as Arc<dyn daemon::Deps>,
    )
    .unwrap();
    (h.with_daemon(Box::new(InProcess { service })), facts)
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

// ---- logs, shell and exec

/// A project with a running sandbox.
fn running() -> (Harness, Arc<Facts>) {
    let (h, facts) = harness_and_facts();
    let started = h.run(&["up", "--all-ports"], "");
    assert_eq!(started.code, 0, "{}", started.all());
    (h, facts)
}

/// A project whose sandbox exists and has been stopped.
fn stopped() -> (Harness, Arc<Facts>) {
    let (h, facts) = running();
    let done = h.run(&["down"], "");
    assert_eq!(done.code, 0, "{}", done.all());
    (h, facts)
}

/// Everything the engine was asked to do after this many earlier calls.
fn calls_since(facts: &Facts, earlier: usize) -> Vec<FakeCall> {
    facts.engine.calls().split_off(earlier)
}

/// The only engine call allowed on a sandbox that is not running is asking what
/// state it is in, since the engine's `exec` boots a stopped one.
fn assert_only_status(calls: &[FakeCall]) {
    assert!(
        calls.iter().all(|call| matches!(call, FakeCall::Status(_))),
        "{calls:?}"
    );
}

fn last_stream_command(facts: &Facts) -> ExecRequest {
    facts
        .engine
        .calls()
        .into_iter()
        .rev()
        .find_map(|call| match call {
            FakeCall::ExecStream(_, request) => Some(request),
            _ => None,
        })
        .expect("nothing was streamed")
}

/// A stand-in for the engine's binary that records how it was started.
struct StubEngine {
    dir: tempfile::TempDir,
}

impl StubEngine {
    fn new(exit_code: i32) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("engine");
        let script = format!(
            "#!/bin/sh\nhere=\"$(dirname \"$0\")\"\nprintf '%s\\n' \"$@\" > \"$here/args\"\npwd > \"$here/cwd\"\nprintf '[%s][%s][%s]' \"$HOME\" \"$USER\" \"$ENGINE_TEST_SECRET\" > \"$here/env\"\nprintf '%s' \"${}\" > \"$here/home\"\nexit {exit_code}\n",
            engine::argv::HOME_VAR
        );
        std::fs::write(&binary, script).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir }
    }

    fn location(&self) -> EngineLocation {
        EngineLocation {
            binary: self.dir.path().join("engine"),
            home: PathBuf::from("/engine/home"),
        }
    }

    fn ran(&self) -> bool {
        self.dir.path().join("args").exists()
    }

    fn args(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("args"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn file(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(name)).unwrap()
    }
}

#[test]
fn logs_run_compose_in_the_project_folder_with_the_default_tail() {
    let (h, facts) = running();
    let done = h.run(&["logs"], "");
    assert_eq!(done.code, 0, "{}", done.all());
    let request = last_stream_command(&facts);
    assert_eq!(
        request.command,
        ["docker", "compose", "logs", "--no-color", "--tail", "200"]
    );
    assert_eq!(request.workdir.unwrap().as_str(), "/work");
    assert!(request.timeout.is_some(), "a plain read has a time limit");
}

#[test]
fn logs_pass_on_a_service_a_tail_and_follow() {
    let (h, facts) = running();
    facts.engine.set_stream_code(0);
    let done = h.run(&["logs", "web.1_x-y", "--tail", "5", "-f"], "");
    assert_eq!(done.code, 0, "{}", done.all());
    let request = last_stream_command(&facts);
    assert_eq!(
        request.command,
        ["docker", "compose", "logs", "--no-color", "--tail", "5", "--follow", "web.1_x-y"]
    );
    assert_eq!(request.timeout, None, "following has no time limit");
}

#[test]
fn logs_can_name_the_project_folder() {
    let (h, facts) = running();
    let elsewhere = tempfile::tempdir().unwrap();
    let done = h.run(&["logs", "--project", elsewhere.path().to_str().unwrap()], "");
    // A different folder is a different project, with no sandbox of its own.
    assert_eq!(done.code, cli::exit::FAILED, "{}", done.all());
    assert!(done.err.contains("no sandbox"), "{}", done.all());
    let root = h.project.path().to_str().unwrap().to_owned();
    assert_eq!(h.run(&["logs", "--project", &root], "").code, 0);
    last_stream_command(&facts);
}

#[test]
fn hostile_log_output_is_filtered_keeping_text_tabs_and_newlines() {
    let (h, facts) = running();
    for chunk in [
        &b"web | hello\tthere\n"[..],
        b"\x1b]0;evil title\x07\x1b[2J\x1b[31mred\x1b[0m\n",
        b"bell\x07 bidi\xe2\x80\xae done\rover\r\n",
        b"split \x1b[3",
        b"1mcolour\x1b",
        b"]52;c;QQ==\x07 euro \xe2\x82",
        b"\xac end\n",
    ] {
        facts.engine.push_stream_output(StreamKind::Stdout, chunk);
    }
    let done = h.run(&["logs"], "");
    assert_eq!(done.code, 0, "{}", done.all());
    assert_eq!(
        done.out,
        "web | hello\tthere\n\
         red\n\
         bell\\u{7} bidi\\u{202e} done\nover\n\
         split colour euro \u{20ac} end\n"
    );
}

#[test]
fn log_errors_go_to_stderr_filtered_and_the_exit_code_shows_failure() {
    let (h, facts) = running();
    facts
        .engine
        .push_stream_output(StreamKind::Stderr, b"no such service: x\x1b[2J\n");
    facts.engine.set_stream_code(1);
    let done = h.run(&["logs", "x"], "");
    assert_eq!(done.code, cli::exit::FAILED, "{}", done.all());
    assert!(done.err.contains("no such service: x\n"), "{}", done.err);
    assert!(!done.err.contains('\u{1b}'), "{:?}", done.err);
    assert!(done.out.is_empty());
}

#[test]
fn a_bad_service_name_is_refused_before_anything_is_asked() {
    let (h, facts) = running();
    let earlier = facts.engine.calls().len();
    for args in [
        &["logs", "--", "-f"][..],
        &["logs", "a;b"],
        &["logs", "a b"],
        &["logs", "$(id)"],
        &["logs", "a/b"],
        &["shell", "--", "-f"],
        &["shell", "a;b"],
        &["exec", "--service=-f", "--", "ls"],
        &["exec", "--service", "a;b", "--", "ls"],
    ] {
        let done = h.run(args, "");
        assert_eq!(done.code, cli::exit::USAGE, "{args:?}: {}", done.all());
    }
    assert_eq!(calls_since(&facts, earlier), []);
}

#[test]
fn too_many_lines_is_refused() {
    let (h, _facts) = running();
    let done = h.run(&["logs", "--tail", "100001"], "");
    assert_eq!(done.code, cli::exit::USAGE, "{}", done.all());
    assert_eq!(h.run(&["logs", "--tail", "nope"], "").code, cli::exit::USAGE);
}

#[test]
fn logs_refuse_a_stopped_sandbox_without_booting_it() {
    for follow in [false, true] {
        let (h, facts) = stopped();
        let earlier = facts.engine.calls().len();
        let args: &[&str] = if follow { &["logs", "-f"] } else { &["logs"] };
        let done = h.run(args, "");
        assert_eq!(done.code, cli::exit::FAILED, "{}", done.all());
        assert!(done.err.contains("stopped"), "{}", done.err);
        assert!(done.err.contains(" up "), "says how to start it: {}", done.err);
        assert_only_status(&calls_since(&facts, earlier));
    }
}

#[test]
fn shell_and_exec_refuse_a_stopped_sandbox_without_booting_it_or_running_the_engine() {
    for args in [
        &["shell"][..],
        &["shell", "web"],
        &["exec", "--", "ls"],
        &["exec", "--service", "web", "--", "ls"],
    ] {
        let (h, facts) = stopped();
        let stub = StubEngine::new(0);
        facts.engine.set_location(stub.location());
        let earlier = facts.engine.calls().len();
        let done = h.run(args, "");
        assert_eq!(done.code, cli::exit::FAILED, "{args:?}: {}", done.all());
        assert!(done.err.contains("stopped"), "{args:?}: {}", done.err);
        assert!(done.err.contains(" up "), "{args:?}: {}", done.err);
        assert!(!stub.ran(), "{args:?} ran the engine");
        assert_only_status(&calls_since(&facts, earlier));
    }
}

#[test]
fn shell_refuses_a_project_with_no_sandbox() {
    let (h, facts) = harness_and_facts();
    let stub = StubEngine::new(0);
    facts.engine.set_location(stub.location());
    let done = h.run(&["shell"], "");
    assert_eq!(done.code, cli::exit::FAILED, "{}", done.all());
    assert!(done.err.contains("no sandbox"), "{}", done.err);
    assert!(!stub.ran());
    assert_only_status(&facts.engine.calls());
}

#[test]
fn shell_runs_the_engine_with_a_cleared_environment_and_prints_the_notice_first() {
    let (h, facts) = running();
    let stub = StubEngine::new(0);
    facts.engine.set_location(stub.location());
    let earlier = facts.engine.calls().len();
    let done = h.run(&["shell"], "");
    assert_eq!(done.code, 0, "{}", done.all());

    // Tests have no terminal, so this takes the no-terminal form.
    let args = stub.args();
    assert_eq!(args[..5], ["exec", "--stream", "--no-tty", "--workdir", "/work"]);
    assert_eq!(args[6..], ["--", "sh"]);
    assert_eq!(stub.file("home"), "/engine/home");
    assert_eq!(stub.file("env"), "[][][]", "the environment was not cleared");
    let cwd = stub.file("cwd");
    assert_eq!(cwd.trim(), "/");

    assert!(done.err.contains("inside the sandbox"), "{}", done.err);
    assert!(done.err.contains("paste"), "{}", done.err);
    assert!(done.err.contains("can't be filtered"), "{}", done.err);
    // Only the state check reached the fake: the terminal itself is the engine.
    assert_only_status(&calls_since(&facts, earlier));
}

#[test]
fn shell_with_a_service_goes_through_compose_from_the_project_folder() {
    let (h, facts) = running();
    let stub = StubEngine::new(0);
    facts.engine.set_location(stub.location());
    assert_eq!(h.run(&["shell", "web"], "").code, 0);
    let args = stub.args();
    assert_eq!(args[..5], ["exec", "--stream", "--no-tty", "--workdir", "/work"]);
    assert_eq!(
        args[6..],
        ["--", "docker", "compose", "exec", "-T", "web", "sh"]
    );
}

#[test]
fn exec_passes_back_the_exit_code_and_keeps_its_arguments_apart() {
    let (h, facts) = running();
    let stub = StubEngine::new(42);
    facts.engine.set_location(stub.location());
    let done = h.run(&["exec", "--", "sh", "-c", "a b;c $(d)"], "");
    assert_eq!(done.code, 42, "{}", done.all());
    assert_eq!(stub.args()[6..], ["--", "sh", "-c", "a b;c $(d)"]);
    assert!(done.err.contains("inside the sandbox"), "{}", done.err);
}

#[test]
fn exec_in_a_service_goes_through_compose() {
    let (h, facts) = running();
    let stub = StubEngine::new(3);
    facts.engine.set_location(stub.location());
    let done = h.run(&["exec", "--service", "api", "--", "ls", "-l"], "");
    assert_eq!(done.code, 3, "{}", done.all());
    assert_eq!(
        stub.args()[6..],
        ["--", "docker", "compose", "exec", "-T", "api", "ls", "-l"]
    );
}

#[test]
fn exec_needs_a_command_after_the_separator() {
    let (h, _facts) = running();
    assert_eq!(h.run(&["exec"], "").code, cli::exit::USAGE);
    assert_eq!(h.run(&["exec", "ls"], "").code, cli::exit::USAGE);
}

/// With no location from the daemon there is nowhere to run, and nothing here
/// may go looking for an engine, even one planted where a search would find it.
#[test]
fn nothing_goes_looking_for_an_engine_when_the_daemon_has_none_to_give() {
    let (h, _facts) = running();
    let planted = h.project.path().join("msb");
    let marker = h.project.path().join("planted-ran");
    std::fs::write(&planted, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
    std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o755)).unwrap();
    let done = h.run(&["shell"], "");
    assert_eq!(done.code, cli::exit::FAILED, "{}", done.all());
    assert!(done.err.contains("cannot be run"), "{}", done.err);
    assert!(!marker.exists());
}

#[test]
fn a_daemon_too_old_to_know_a_request_is_explained() {
    let error = CallError::Refused(proto::ApiError::new(
        proto::ErrorCode::BadRequest,
        proto::WireError::Malformed.to_string(),
    ));
    let message = error.message();
    assert!(message.contains("older"), "{message}");
    assert!(message.contains(brand::DAEMON_BIN), "{message}");
    assert!(message.contains("keep running"), "{message}");
}

/// A small daemon on a real socket, because a time limit only means something
/// against a daemon that really does not answer.
fn listener() -> (tempfile::TempDir, UnixListener) {
    let dir = tempfile::Builder::new().prefix("s").tempdir_in("/tmp").unwrap();
    let listener = UnixListener::bind(dir.path().join(brand::SOCKET_FILE)).unwrap();
    (dir, listener)
}

fn status_request() -> Request {
    Request::Status(proto::ProjectArgs {
        project: "/p".to_owned(),
    })
}

#[test]
fn a_daemon_that_never_answers_ends_at_the_time_limit() {
    let (dir, listener) = listener();
    let _held = std::thread::spawn(move || {
        let connection = listener.accept();
        std::thread::sleep(Duration::from_secs(30));
        drop(connection);
    });
    let client = cli::SocketClient::new(dir.path());
    let started = Instant::now();
    let result = client.stream(
        status_request(),
        &mut |_, _| true,
        Some(Duration::from_millis(200)),
    );
    assert!(matches!(result, Err(CallError::TimedOut)), "{:?}", result.err().map(|e| e.message()));
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
}

#[test]
fn a_daemon_that_keeps_talking_still_ends_at_the_time_limit() {
    let (dir, listener) = listener();
    let _held = std::thread::spawn(move || {
        let (mut connection, _) = listener.accept().unwrap();
        let frame = proto::Frame::Output {
            stream: proto::OutputStream::Stdout,
            text: "more\n".to_owned(),
        };
        while proto::write_frame(&mut connection, &frame).is_ok() {
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let client = cli::SocketClient::new(dir.path());
    let started = Instant::now();
    let result = client.stream(
        status_request(),
        &mut |_, _| true,
        Some(Duration::from_millis(300)),
    );
    assert!(matches!(result, Err(CallError::TimedOut)));
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
}

#[test]
fn a_reader_that_stops_ends_the_call() {
    let (dir, listener) = listener();
    let _held = std::thread::spawn(move || {
        let (mut connection, _) = listener.accept().unwrap();
        let frame = proto::Frame::Output {
            stream: proto::OutputStream::Stdout,
            text: "more\n".to_owned(),
        };
        while proto::write_frame(&mut connection, &frame).is_ok() {}
    });
    let client = cli::SocketClient::new(dir.path());
    let result = client.stream(status_request(), &mut |_, _| false, None);
    assert!(matches!(result, Err(CallError::Stopped)));
}
