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
    /// Every fetch asked for: URL, ref and agent socket.
    fetches: std::sync::Mutex<Vec<(String, String, Option<PathBuf>)>>,
    /// What a fetch fails with, if it should.
    fetch_error: std::sync::Mutex<Option<String>>,
    /// A bare repository standing in for the remote. With one, the relay's
    /// `ssh` runs `git upload-pack` or `git receive-pack` on it directly.
    git_remote: std::sync::Mutex<Option<PathBuf>>,
    /// Every relay connection: service and agent socket.
    relayed: std::sync::Mutex<Vec<(gitbroker::GitService, Option<PathBuf>)>>,
    /// A stand-in for the person at the keyboard: `None` means this computer
    /// cannot ask anyone, `Some(answer)` that they are asked and answer so.
    person: std::sync::Mutex<Option<bool>>,
    /// Every reason the person was asked to confirm, in order.
    asked: std::sync::Mutex<Vec<String>>,
}

/// Runs system Git to set up or inspect a test repository. The code under
/// test never runs Git on the host this way.
fn test_git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Remote")
        .env("GIT_AUTHOR_EMAIL", "remote@example.invalid")
        .env("GIT_COMMITTER_NAME", "Remote")
        .env("GIT_COMMITTER_EMAIL", "remote@example.invalid")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

/// The commit a fake fetch reports.
const FETCHED: &str = "0123456789abcdef0123456789abcdef01234567";

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

    /// Stands in for git over ssh: makes the mirror's HEAD name a commit, as
    /// the real fetch leaves it, and records what was asked.
    fn fetch_remote(
        &self,
        mirror: &Path,
        url: &str,
        git_ref: &str,
        agent_socket: Option<&Path>,
    ) -> Result<String, String> {
        self.fetches.lock().unwrap().push((
            url.to_owned(),
            git_ref.to_owned(),
            agent_socket.map(Path::to_path_buf),
        ));
        if let Some(error) = self.fetch_error.lock().unwrap().clone() {
            return Err(error);
        }
        let commit = match self.git_remote.lock().unwrap().clone() {
            Some(bare) => test_git(&bare, &["rev-parse", "HEAD"]).trim().to_owned(),
            None => FETCHED.to_owned(),
        };
        std::fs::create_dir_all(mirror.join("objects")).unwrap();
        std::fs::write(mirror.join("HEAD"), format!("{commit}\n")).unwrap();
        Ok(commit)
    }

    fn person_check(&self) -> Option<&'static str> {
        self.person.lock().unwrap().map(|_| "a test fingerprint")
    }

    fn confirm(&self, reason: &str) -> personcheck::Outcome {
        self.asked.lock().unwrap().push(reason.to_owned());
        match *self.person.lock().unwrap() {
            Some(true) => personcheck::Outcome::Confirmed,
            _ => personcheck::Outcome::Declined("you cancelled".to_owned()),
        }
    }

    fn spawn_ssh(
        &self,
        _url: &str,
        service: gitbroker::GitService,
        git_protocol: Option<&str>,
        agent_socket: Option<&Path>,
    ) -> Result<std::process::Child, String> {
        self.relayed.lock().unwrap().push((service, agent_socket.map(Path::to_path_buf)));
        let bare = self.git_remote.lock().unwrap().clone().ok_or("no test remote")?;
        let sub = match service {
            gitbroker::GitService::UploadPack => "upload-pack",
            gitbroker::GitService::ReceivePack => "receive-pack",
        };
        let mut command = std::process::Command::new("git");
        command
            .arg(sub)
            .arg(&bare)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(protocol) = git_protocol {
            command.env("GIT_PROTOCOL", protocol);
        }
        command.spawn().map_err(|e| e.to_string())
    }

    fn snapshot(
        &self,
        root: &Path,
        source: &Source,
        dest: &Path,
    ) -> Result<SnapshotPlan, String> {
        // The real snapshot creates its destination and refuses one that exists.
        std::fs::create_dir(dest).unwrap();
        std::fs::write(dest.join("compose.yaml"), b"services: {}\n").unwrap();
        if root.ends_with("mirror.git") {
            // Exporting a fetched remote's commit: give it a small project.
            std::fs::write(dest.join("compose.yaml"), "services:\n  web:\n    image: web:1\n").unwrap();
            std::fs::write(dest.join(brand::CONFIG_FILE), "resources:\n  memory: 4G\n  cpus: 2\n  disk: 30G\n").unwrap();
        }
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
        fetches: std::sync::Mutex::new(Vec::new()),
        fetch_error: std::sync::Mutex::new(None),
        git_remote: std::sync::Mutex::new(None),
        relayed: std::sync::Mutex::new(Vec::new()),
        person: std::sync::Mutex::new(None),
        asked: std::sync::Mutex::new(Vec::new()),
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
fn status_says_what_is_not_recorded_rather_than_showing_none() {
    let h = harness();
    let text = h.run(&["status"], "").all();
    assert!(text.contains("nothing is being recorded: no sandbox"), "{text}");
    assert!(text.contains("Raw IP, metadata and UDP"), "{text}");
    assert!(!text.contains("0 blocked"), "{text}");
}

/// A project started through the real daemon logic, whose fake engine keeps the
/// files the real one keeps, in a folder that lasts as long as the test.
fn recording() -> (Harness, Arc<Facts>, tempfile::TempDir, engine::SandboxName) {
    let (h, facts) = harness_and_facts();
    let home = tempfile::tempdir().unwrap();
    facts.engine.set_location(EngineLocation {
        binary: PathBuf::from("/nonexistent"),
        home: home.path().join("msb"),
    });
    facts.engine.keep_files();
    let done = h.run(&["up", "--all-ports"], "");
    assert_eq!(done.code, 0, "{}", done.all());
    let name = facts
        .engine
        .calls()
        .iter()
        .find_map(|call| match call {
            FakeCall::Create(spec) => Some(spec.name.clone()),
            _ => None,
        })
        .unwrap();
    (h, facts, home, name)
}

fn drop_recorded_level(h: &Harness) {
    let projects = h.state().join("projects");
    for entry in std::fs::read_dir(projects).unwrap() {
        let path = entry.unwrap().path().join("sandbox.json");
        if path.exists() {
            let text = std::fs::read_to_string(&path).unwrap();
            let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
            value.as_object_mut().unwrap().remove("log_level");
            std::fs::write(&path, value.to_string()).unwrap();
        }
    }
}

#[test]
fn net_blocked_shows_time_count_and_name_with_the_newest_last() {
    let (h, facts, _home, name) = recording();
    for domain in ["old.example.net", "new.example.net", "OLD.example.net", "new.example.net", "new.example.net"] {
        facts.engine.deny_lookup(&name, domain).unwrap();
    }
    let done = h.run(&["net", "blocked"], "");
    assert_eq!(done.code, 0, "{}", done.all());
    let text = done.out;
    assert!(text.contains("HOSTNAME"), "{text}");
    let old = text.find("old.example.net").expect(&text);
    let new = text.find("new.example.net").expect(&text);
    assert!(old < new, "the newest must be last: {text}");
    assert!(text.contains("2026-11-05 07:50:00"), "{text}");
    assert!(text.lines().any(|l| l.contains("old.example.net") && l.contains(" 2  ")), "{text}");
    assert!(text.lines().any(|l| l.contains("new.example.net") && l.contains(" 3  ")), "{text}");
    assert!(text.contains("Raw IP, metadata and UDP"), "{text}");
    assert!(text.contains("doesn't mean nothing was tried"), "{text}");
    assert!(text.contains("the guest cannot write"), "{text}");
}

#[test]
fn net_blocked_says_the_list_is_empty_only_as_far_as_it_goes() {
    let (h, _facts, _home, _name) = recording();
    let text = h.run(&["net", "blocked"], "").all();
    assert!(text.contains("No blocked hostnames are recorded so far"), "{text}");
    assert!(text.contains("doesn't mean nothing was tried"), "{text}");
}

/// A sandbox made before the level was kept: nothing is known to be recording,
/// even if its log holds what looks like events, and nothing is written to its
/// record as a guess.
#[test]
fn a_sandbox_with_no_recorded_level_is_not_recording_and_stays_that_way() {
    let (h, facts, _home, name) = recording();
    facts.engine.deny_lookup(&name, "there.example.net").unwrap();
    drop_recorded_level(&h);

    let done = h.run(&["net", "blocked"], "");
    assert_eq!(done.code, 0, "{}", done.all());
    let text = done.all();
    assert!(text.contains("Nothing is being recorded"), "{text}");
    assert!(text.contains("down --destroy"), "{text}");
    assert!(text.contains("deletes its data"), "{text}");
    assert!(!text.contains("there.example.net"), "{text}");
    assert!(!text.contains("recorded so far"), "{text}");

    let status = h.run(&["status"], "").all();
    assert!(status.contains("Nothing is being recorded"), "{status}");
    assert!(!status.contains("refused lookups"), "{status}");

    let projects = h.state().join("projects");
    for entry in std::fs::read_dir(projects).unwrap() {
        let path = entry.unwrap().path().join("sandbox.json");
        if path.exists() {
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(!text.contains("log_level"), "something guessed: {text}");
        }
    }
}

#[test]
fn following_something_that_can_never_show_anything_says_so_and_stops() {
    let (h, _facts, _home, _name) = recording();
    drop_recorded_level(&h);
    let done = h.run(&["net", "blocked", "-f"], "");
    assert_eq!(done.code, cli::exit::FAILED, "{}", done.all());
    assert!(done.err.contains("not following"), "{}", done.all());
    assert!(done.out.contains("Nothing is being recorded"), "{}", done.all());
}

#[test]
fn a_new_sandbox_is_recorded_as_created_at_the_level_that_records() {
    let (h, facts, _home, name) = recording();
    assert!(matches!(
        facts.engine.calls().iter().find(|c| matches!(c, FakeCall::Create(_))),
        Some(FakeCall::Create(spec)) if engine::argv::create(spec)
            .unwrap()
            .windows(2)
            .any(|pair| pair[0] == "--log-level" && pair[1] == "debug")
    ));
    facts.engine.deny_lookup(&name, "a.example.net").unwrap();
    let text = h.run(&["status"], "").all();
    assert!(text.contains("1 refused lookups of 1 hostnames"), "{text}");
    assert!(text.contains("Raw IP, metadata and UDP"), "{text}");
}

#[test]
fn a_link_at_the_log_is_refused_and_status_keeps_working() {
    let (h, facts, home, name) = recording();
    let logs = facts.engine.logs_dir(&name).unwrap();
    let planted = home.path().join("planted");
    std::fs::write(&planted, "x\n").unwrap();
    std::os::unix::fs::symlink(&planted, logs.join("runtime.log")).unwrap();

    let done = h.run(&["net", "blocked"], "");
    assert_eq!(done.code, cli::exit::FAILED, "{}", done.all());
    assert!(done.err.contains("symbolic link"), "{}", done.all());

    let status = h.run(&["status"], "");
    assert_eq!(status.code, 0, "{}", status.all());
    assert!(status.all().contains("could not be read"), "{}", status.all());
    assert!(status.all().contains("running"), "{}", status.all());
}

#[test]
fn a_hostile_name_on_a_real_shaped_line_is_shown_harmlessly_or_not_at_all() {
    let (h, facts, _home, name) = recording();
    facts.engine.deny_lookup(&name, r"a\040domain\=fake.example.net").unwrap();
    facts.engine.deny_lookup(&name, "a b\u{1b}[2J.example.net").unwrap();
    let done = h.run(&["net", "blocked"], "");
    assert_eq!(done.code, 0, "{}", done.all());
    let text = done.all();
    assert!(!text.contains('\u{1b}'), "{text}");
    assert!(text.contains(r"a\040domain\=fake.example.net"), "{text}");
    assert!(text.contains("1 log lines looked like blocked events"), "{text}");
}

/// Answers the first calls, then stops, so that following can be shown to end.
struct Scripted {
    answers: std::sync::Mutex<std::collections::VecDeque<Result<Reply, CallError>>>,
    limits: std::sync::Mutex<Vec<Option<Duration>>>,
    afters: std::sync::Mutex<Vec<Option<proto::BlockedCursor>>>,
}

impl DaemonClient for Scripted {
    fn call(
        &self,
        request: Request,
        _progress: &mut dyn FnMut(&str, Option<&str>),
    ) -> Result<Reply, CallError> {
        self.stream(request, &mut |_, _| true, None)
    }

    fn stream(
        &self,
        request: Request,
        _output: &mut dyn FnMut(proto::OutputStream, &str) -> bool,
        limit: Option<Duration>,
    ) -> Result<Reply, CallError> {
        self.limits.lock().unwrap().push(limit);
        if let Request::Blocked(args) = request {
            self.afters.lock().unwrap().push(args.after);
        }
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(CallError::Transport("the script ended".to_owned())))
    }
}

fn blocked_answer(domain: Option<&str>, offset: u64) -> Result<Reply, CallError> {
    Ok(Reply::Blocked(proto::BlockedReport {
        recording: proto::BlockedRecording::Recording,
        log_level: Some("debug".to_owned()),
        events: domain
            .map(|domain| proto::BlockedEvent {
                time: "2026-11-05T07:50:52.406636Z".to_owned(),
                domain: domain.to_owned(),
                count: 1,
            })
            .into_iter()
            .collect(),
        total: u64::from(domain.is_some()),
        cut_off: false,
        omitted_domains: 0,
        rejected_lines: 0,
        unlisted_domains: 0,
        trust: proto::LogTrust::HostOnly,
        next: Some(proto::BlockedCursor { file: 1, offset }),
    }))
}

#[test]
fn following_asks_from_where_it_left_off_prints_only_what_is_new_and_has_a_limit_each_time() {
    let script = Arc::new(Scripted {
        answers: std::sync::Mutex::new(
            [
                blocked_answer(Some("first.example.net"), 100),
                blocked_answer(None, 100),
                Err(CallError::TimedOut),
                blocked_answer(Some("second.example.net"), 200),
            ]
            .into_iter()
            .collect(),
        ),
        limits: std::sync::Mutex::default(),
        afters: std::sync::Mutex::default(),
    });
    struct Shared(Arc<Scripted>);
    impl DaemonClient for Shared {
        fn call(
            &self,
            request: Request,
            progress: &mut dyn FnMut(&str, Option<&str>),
        ) -> Result<Reply, CallError> {
            self.0.call(request, progress)
        }
        fn stream(
            &self,
            request: Request,
            output: &mut dyn FnMut(proto::OutputStream, &str) -> bool,
            limit: Option<Duration>,
        ) -> Result<Reply, CallError> {
            self.0.stream(request, output, limit)
        }
    }
    let h = Harness::new().with_daemon(Box::new(Shared(Arc::clone(&script))));
    let done = h.run(&["net", "blocked", "-f"], "");
    assert_eq!(done.code, cli::exit::FAILED, "{}", done.all());
    assert_eq!(done.out.matches("first.example.net").count(), 1, "{}", done.out);
    assert_eq!(done.out.matches("second.example.net").count(), 1, "{}", done.out);
    assert!(done.err.contains("Still trying"), "{}", done.err);
    assert!(done.err.contains("the script ended"), "{}", done.err);
    assert!(script.limits.lock().unwrap().iter().all(Option::is_some));
    let offsets: Vec<_> = script
        .afters
        .lock()
        .unwrap()
        .iter()
        .map(|after| after.map(|cursor| cursor.offset))
        .collect();
    assert_eq!(offsets, [None, Some(100), Some(100), Some(100), Some(200)]);
}

#[test]
fn status_on_an_older_daemon_says_the_count_is_unknown_rather_than_zero() {
    struct Older;
    impl DaemonClient for Older {
        fn call(
            &self,
            _request: Request,
            _progress: &mut dyn FnMut(&str, Option<&str>),
        ) -> Result<Reply, CallError> {
            Ok(Reply::Status(proto::StatusReport {
                project_id: "p".to_owned(),
                project_path: "/p".to_owned(),
                sandbox: proto::SandboxState::Running,
                services: Vec::new(),
                ports: Vec::new(),
                resources: None,
                blocked_count: 0,
                blocked: None,
                exposure: None,
                usage: None,
            }))
        }
        fn stream(
            &self,
            _request: Request,
            _output: &mut dyn FnMut(proto::OutputStream, &str) -> bool,
            _limit: Option<Duration>,
        ) -> Result<Reply, CallError> {
            unreachable!("status has no output")
        }
    }
    let h = Harness::new().with_daemon(Box::new(Older));
    let text = h.run(&["status"], "").all();
    assert!(text.contains("unknown"), "{text}");
    assert!(text.contains("older than this command"), "{text}");
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
fn a_port_moved_with_ports_is_used_by_the_next_start() {
    let h = harness();
    assert_eq!(h.run(&["up", "--all-ports"], "").code, 0);
    let mac = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();

    let saved = h.run(&["ports", "open", "web:3000", "--on", &mac.to_string()], "");
    assert_eq!(saved.code, 0, "{}", saved.all());
    let text = saved.all();
    assert!(text.contains("Saved. Nothing changes until it next starts"), "{text}");
    assert!(text.contains(&format!("open on 127.0.0.1:{mac}, now open on 127.0.0.1:3000")), "{text}");
    assert!(text.contains("Restart it to apply them"), "{text}");

    let unknown = h.run(&["ports", "close", "9999"], "");
    assert_eq!(unknown.code, 2, "{}", unknown.all());
    assert!(unknown.err.contains("is not a port this project publishes"), "{}", unknown.all());

    assert_eq!(h.run(&["down"], "").code, 0);
    let started = h.run(&["up"], "").all();
    assert!(started.contains(&format!("Forwarding {mac}")), "{started}");
    assert!(started.contains("as chosen before"), "{started}");
    let status = h.run(&["status"], "").all();
    assert!(status.contains(&format!("127.0.0.1:{mac}")), "{status}");
    assert!(!status.contains("127.0.0.1:3000"), "{status}");
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

// ---- mcp: an agent on the host working in the sandbox

/// A stand-in for the engine's binary that records its arguments, its stdin
/// and its environment, and prints something hostile on both streams.
struct EchoEngine {
    dir: tempfile::TempDir,
}

impl EchoEngine {
    fn new(exit_code: i32) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("engine");
        let script = format!(
            "#!/bin/sh\nhere=\"$(dirname \"$0\")\"\nprintf '%s\\n' \"$@\" > \"$here/args\"\ncat > \"$here/stdin\"\nprintf '[%s][%s]' \"$HOME\" \"$USER\" > \"$here/env\"\nprintf 'out\\033[2J\\033]0;title\\007ok\\n'\nprintf 'err\\n' >&2\nexit {exit_code}\n"
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

    /// Everything after the sandbox's name and `--`: what ran inside it.
    fn inside(&self) -> Vec<String> {
        let args = self.file("args");
        let args: Vec<String> = args.lines().map(str::to_owned).collect();
        assert_eq!(args[..5], ["exec", "--stream", "--no-tty", "--workdir", "/work"]);
        assert_eq!(args[6], "--");
        args[7..].to_vec()
    }

    fn file(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(name)).unwrap()
    }
}

fn mcp_line(id: u64, method: &str, params: serde_json::Value) -> String {
    let mut line = serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
        .to_string();
    line.push('\n');
    line
}

fn tool_call(id: u64, name: &str, arguments: serde_json::Value) -> String {
    mcp_line(id, "tools/call", serde_json::json!({ "name": name, "arguments": arguments }))
}

/// Starts `mcp serve` for the harness's project, as an agent would, sends the
/// handshake and then `calls`, and returns each answer by its id.
fn serve(h: &Harness, calls: &[String]) -> Vec<serde_json::Value> {
    let mut input = mcp_line(
        0,
        "initialize",
        serde_json::json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } }),
    );
    input.push_str("{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n");
    for call in calls {
        input.push_str(call);
    }
    let project = h.project.path().to_str().unwrap().to_owned();
    let done = h.run(&["mcp", "serve", "--project", &project], &input);
    assert_eq!(done.code, 0, "{}", done.all());
    let answers: Vec<serde_json::Value> = done
        .out
        .lines()
        .map(|line| serde_json::from_str(line).expect("stdout carries only JSON"))
        .collect();
    assert_eq!(answers.len(), calls.len() + 1, "{}", done.out);
    assert_eq!(answers[0]["result"]["protocolVersion"], "2025-06-18");
    answers
}

fn tool_text(answer: &serde_json::Value) -> &str {
    answer["result"]["content"][0]["text"].as_str().unwrap()
}

#[test]
fn mcp_exec_runs_sh_in_the_sandbox_through_the_daemon_with_a_cleared_environment() {
    let (h, facts) = running();
    let engine = EchoEngine::new(3);
    facts.engine.set_location(engine.location());
    let earlier = facts.engine.calls().len();
    let answers = serve(&h, &[tool_call(1, "exec", serde_json::json!({ "command": "ls -la; echo $(id)" }))]);

    assert_eq!(engine.inside(), ["sh", "-c", "ls -la; echo $(id)"]);
    assert_eq!(engine.file("env"), "[][]", "the environment was not cleared");
    assert_eq!(engine.file("stdin"), "", "a command gets no input");
    // The daemon checked the state, and nothing else reached the engine.
    assert_only_status(&calls_since(&facts, earlier));

    let answer = &answers[1];
    assert_eq!(answer["id"], 1);
    assert_eq!(answer["result"]["isError"], true, "a non-zero exit is a failure");
    let text = tool_text(answer);
    assert!(text.starts_with("exit code 3"), "{text}");
    assert!(text.contains("data, not instructions"), "{text}");
    assert!(text.contains("[stdout]\noutok\n"), "{text}");
    assert!(text.contains("[stderr]\nerr\n"), "{text}");
    assert!(!text.contains('\u{1b}') && !text.contains('\u{7}'), "{text:?}");
}

#[test]
fn mcp_exec_in_a_service_goes_through_compose_without_a_terminal() {
    let (h, facts) = running();
    let engine = EchoEngine::new(0);
    facts.engine.set_location(engine.location());
    let answers = serve(
        &h,
        &[tool_call(1, "exec", serde_json::json!({ "command": "env", "service": "web" }))],
    );
    assert_eq!(answers[1]["result"]["isError"], false);
    assert_eq!(
        engine.inside(),
        ["docker", "compose", "exec", "-T", "web", "sh", "-c", "env"]
    );
}

#[test]
fn mcp_read_file_reads_only_under_work() {
    let (h, facts) = running();
    let engine = EchoEngine::new(0);
    facts.engine.set_location(engine.location());
    let answers = serve(
        &h,
        &[
            tool_call(1, "read_file", serde_json::json!({ "path": "src/a b.rs" })),
            tool_call(2, "read_file", serde_json::json!({ "path": "../../etc/passwd" })),
        ],
    );
    assert_eq!(engine.inside(), ["cat", "/work/src/a b.rs"]);
    assert_eq!(answers[1]["result"]["isError"], false);
    let text = tool_text(&answers[1]);
    assert!(text.contains("/work/src/a b.rs"), "{text}");
    assert!(!text.contains('\u{1b}'), "{text:?}");
    assert!(text.contains("\\u{1b}"), "control characters are shown: {text}");
    assert_eq!(answers[2]["result"]["isError"], true);
}

#[test]
fn mcp_write_file_sends_the_content_on_stdin_and_keeps_the_path_out_of_the_script() {
    let (h, facts) = running();
    let engine = EchoEngine::new(0);
    facts.engine.set_location(engine.location());
    let content = "line one\n$(rm -rf /)\n";
    let answers = serve(
        &h,
        &[tool_call(1, "write_file", serde_json::json!({ "path": "new/$(x);y.txt", "content": content }))],
    );
    assert_eq!(answers[1]["result"]["isError"], false, "{}", answers[1]);
    let inside = engine.inside();
    assert_eq!(inside[..2], ["sh", "-c"]);
    assert!(!inside[2].contains("$(x)"), "the path is not in the script");
    assert_eq!(inside[3..], ["sh", "/work/new/$(x);y.txt"]);
    assert_eq!(engine.file("stdin"), content);
}

#[test]
fn mcp_tools_refuse_a_stopped_sandbox_without_booting_it_or_running_the_engine() {
    let (h, facts) = stopped();
    let engine = EchoEngine::new(0);
    facts.engine.set_location(engine.location());
    let earlier = facts.engine.calls().len();
    let answers = serve(
        &h,
        &[
            tool_call(1, "exec", serde_json::json!({ "command": "ls" })),
            tool_call(2, "read_file", serde_json::json!({ "path": "a" })),
            tool_call(3, "write_file", serde_json::json!({ "path": "a", "content": "x" })),
            tool_call(4, "logs", serde_json::json!({})),
        ],
    );
    for answer in &answers[1..] {
        assert_eq!(answer["result"]["isError"], true, "{answer}");
        let text = tool_text(answer);
        assert!(text.contains("stopped"), "{text}");
        assert!(text.contains(" up "), "says how to start it: {text}");
    }
    assert!(!engine.ran());
    assert_only_status(&calls_since(&facts, earlier));
}

#[test]
fn mcp_status_and_logs_come_from_the_daemon() {
    let (h, facts) = running();
    let answers = serve(
        &h,
        &[
            tool_call(1, "status", serde_json::json!({})),
            tool_call(2, "logs", serde_json::json!({ "service": "web", "tail": 50 })),
        ],
    );
    let status = tool_text(&answers[1]);
    assert!(status.contains("Sandbox: running"), "{status}");
    assert!(status.contains("3000"), "{status}");
    assert_eq!(answers[2]["result"]["isError"], false, "{}", answers[2]);
    let request = last_stream_command(&facts);
    assert_eq!(
        request.command,
        ["docker", "compose", "logs", "--no-color", "--tail", "50", "web"]
    );
}

#[test]
fn mcp_keeps_going_after_a_line_that_is_not_json() {
    let (h, _facts) = running();
    let answers = serve(
        &h,
        &[
            "this is not json\n".to_owned(),
            mcp_line(1, "ping", serde_json::json!({})),
        ],
    );
    assert_eq!(answers[1]["error"]["code"], -32700);
    assert_eq!(answers[2]["id"], 1);
    assert_eq!(answers[2]["result"], serde_json::json!({}));
}

#[test]
fn mcp_serve_needs_a_project_that_exists() {
    let h = harness();
    let done = h.run(&["mcp", "serve"], "");
    assert_eq!(done.code, cli::exit::USAGE, "{}", done.all());
    let done = h.run(&["mcp", "serve", "--project", "/nonexistent/project"], "");
    assert_eq!(done.code, cli::exit::FAILED, "{}", done.all());
    assert!(done.out.is_empty(), "stdout is for the protocol only");
}

#[test]
fn mcp_setup_claude_makes_a_folder_outside_the_project_with_host_tools_denied() {
    let h = harness();
    let parent = tempfile::tempdir().unwrap();
    let folder = parent.path().join("agent");
    let folder_text = folder.to_str().unwrap().to_owned();
    let done = h.run(&["mcp", "setup", "claude", &folder_text, "--binary", "/bin/sh"], "");
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(done.out.contains("claude"), "{}", done.out);

    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(folder.join(".mcp.json")).unwrap()).unwrap();
    let server = &mcp["mcpServers"][brand::CLI_BIN];
    let project = std::fs::canonicalize(h.project.path()).unwrap();
    assert_eq!(
        server["args"],
        serde_json::json!(["mcp", "serve", "--project", project.to_str().unwrap()])
    );
    assert_eq!(server["command"], std::fs::canonicalize("/bin/sh").unwrap().to_str().unwrap());
    // The harness points the state folder somewhere, and the agent's server
    // has to find the same daemon.
    assert_eq!(server["env"][common::home_var()], h.home.path().to_str().unwrap());

    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(folder.join(".claude/settings.json")).unwrap(),
    )
    .unwrap();
    assert!(settings["permissions"]["deny"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("Bash")));
    assert!(std::fs::read_to_string(folder.join("CLAUDE.md")).unwrap().contains("/work"));

    // Running it again would mix with what is there, so it is refused.
    let again = h.run(&["mcp", "setup", "claude", &folder_text, "--binary", "/bin/sh"], "");
    assert_eq!(again.code, cli::exit::FAILED, "{}", again.all());
    assert!(again.err.contains("not empty"), "{}", again.err);
}

#[test]
fn mcp_setup_claude_refuses_a_folder_that_overlaps_the_project() {
    let h = harness();
    let inside = h.project.path().join("agent");
    let done = h.run(
        &["mcp", "setup", "claude", inside.to_str().unwrap(), "--binary", "/bin/sh"],
        "",
    );
    assert_eq!(done.code, cli::exit::FAILED, "{}", done.all());
    assert!(done.err.contains("overlaps the project"), "{}", done.err);
    assert!(!inside.exists(), "the folder it made was removed again");
}

#[test]
fn mcp_setup_codex_prints_the_commands_and_changes_nothing() {
    let h = harness();
    let before: Vec<_> = std::fs::read_dir(h.home.path()).unwrap().collect();
    let done = h.run(&["mcp", "setup", "codex", "--binary", "/bin/sh"], "");
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(done.out.contains(&format!("codex mcp add {}", brand::CLI_BIN)), "{}", done.out);
    assert!(done.out.contains("--sandbox read-only"), "{}", done.out);
    assert!(done.out.contains("mcp serve --project"), "{}", done.out);
    assert!(done.out.contains("Nothing was changed"), "{}", done.out);
    let after: Vec<_> = std::fs::read_dir(h.home.path()).unwrap().collect();
    assert_eq!(before.len(), after.len());
}

// ---- projects fetched from an SSH remote

const REMOTE: &str = "git@example.invalid:org/shop.git";

fn fetches(facts: &Facts) -> Vec<(String, String, Option<PathBuf>)> {
    facts.fetches.lock().unwrap().clone()
}

#[test]
fn up_with_an_ssh_remote_fetches_on_the_host_then_starts_from_that_commit() {
    let w = remote_setup();
    let (h, facts) = (&w.h, &w.facts);
    let head = test_git(&w.bare, &["rev-parse", "HEAD"]).trim().to_owned();
    let done = h.run(&["up", REMOTE, "--all-ports"], "");
    assert_eq!(done.code, 0, "{}", done.all());
    assert_eq!(fetches(facts), [(REMOTE.to_owned(), "HEAD".to_owned(), None)]);
    assert!(done.out.contains("Fetched"), "{}", done.out);
    assert!(done.out.contains(&head), "{}", done.out);
    assert!(done.out.contains("key"), "says where the key stays: {}", done.out);

    // The files live in the daemon's own folder, with a `.git` pointing at
    // the mirror, and that commit is what the sandbox was given.
    let remote = h.state().join("remote");
    let folder = std::fs::read_dir(&remote).unwrap().next().unwrap().unwrap().path();
    let name = folder.file_name().unwrap().to_str().unwrap().to_owned();
    assert!(name.starts_with("example-invalid-shop-"), "{name}");
    let root = folder.join("shop");
    let pointer = std::fs::read_to_string(root.join(".git")).unwrap();
    assert_eq!(pointer.trim(), format!("gitdir: {}", folder.join("mirror.git").display()));
    assert_eq!(daemon::gitref::head_commit(&root).unwrap(), head);
    assert_eq!(test_git(&w.work, &["rev-parse", "HEAD"]).trim(), head, "the sandbox was cloned at that commit");

    // Other commands take the remote in place of a folder.
    let status = h.run(&["status", REMOTE], "");
    assert_eq!(status.code, 0, "{}", status.all());
    assert!(status.out.contains("running"), "{}", status.out);
    assert_eq!(fetches(facts).len(), 1, "looking a remote up does not fetch");
}

#[test]
fn up_passes_on_the_ref_asked_for() {
    let w = remote_setup();
    let (h, facts) = (&w.h, &w.facts);
    let done = h.run(&["up", REMOTE, "--ref", "release/1.2", "--all-ports"], "");
    assert_eq!(done.code, 0, "{}", done.all());
    assert_eq!(fetches(facts)[0].1, "release/1.2");
    assert_eq!(test_git(&w.work, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(), "release/1.2");
}

#[test]
fn a_remote_is_never_fetched_again_under_an_existing_sandbox() {
    let w = remote_setup();
    let (h, facts) = (&w.h, &w.facts);
    assert_eq!(h.run(&["up", REMOTE, "--all-ports"], "").code, 0);
    assert_eq!(h.run(&["down", REMOTE], "").code, 0);
    let again = h.run(&["up", REMOTE, "--ref", "other", "--all-ports"], "");
    assert_eq!(again.code, 0, "{}", again.all());
    assert_eq!(fetches(facts).len(), 1, "fetched again under a sandbox");
    assert!(again.out.contains("not fetched again"), "{}", again.out);
    assert!(again.out.contains("--ref was not used"), "{}", again.out);
    assert!(again.out.contains("git pull"), "{}", again.out);

    // Once it is destroyed, the next up fetches afresh. A real destroy takes
    // /work with it; the stand-in has to be emptied by hand.
    assert_eq!(h.run(&["down", REMOTE, "--destroy"], "").code, 0);
    std::fs::remove_dir_all(&w.work).unwrap();
    std::fs::create_dir(&w.work).unwrap();
    assert_eq!(h.run(&["up", REMOTE, "--all-ports"], "").code, 0);
    assert_eq!(fetches(facts).len(), 2);
}

#[test]
fn a_failed_fetch_says_what_to_do_and_starts_nothing() {
    let (h, facts) = harness_and_facts();
    *facts.fetch_error.lock().unwrap() =
        Some("git@example.invalid: Permission denied (publickey).".to_owned());
    let done = h.run(&["up", REMOTE, "--all-ports"], "");
    assert_eq!(done.code, cli::exit::FAILED, "{}", done.all());
    assert!(done.err.contains("Permission denied"), "{}", done.err);
    assert!(done.err.contains("ssh-add"), "{}", done.err);
    assert!(facts.engine.calls().iter().all(|call| matches!(call, FakeCall::Status(_))), "{:?}", facts.engine.calls());
    // The refusal is on record, and the folder this fetch made is gone again.
    let log = std::fs::read_to_string(h.state().join(hoststate::DAEMON_AUDIT_FILE)).unwrap_or_default();
    assert!(log.contains("remote_fetch_failed") && log.contains(REMOTE), "{log}");
    assert!(log.contains("No key was accepted"), "{log}");
    let left: Vec<_> = std::fs::read_dir(h.state().join("remote")).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_failed_fetch_keeps_a_mirror_that_was_already_there() {
    let w = relay_world();
    let (h, facts) = (&w.h, &w.facts);
    let down = h.run(&["down", REMOTE, "--destroy"], "y\n");
    assert_eq!(down.code, 0, "{}", down.all());
    let before: Vec<_> = std::fs::read_dir(h.state().join("remote")).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(before.len(), 1);
    *facts.fetch_error.lock().unwrap() = Some("Host key verification failed.".to_owned());
    let done = h.run(&["up", REMOTE, "--all-ports"], "");
    assert_eq!(done.code, cli::exit::FAILED, "{}", done.all());
    assert!(before[0].join("mirror.git").join("HEAD").is_file(), "the earlier mirror was kept");
}

#[test]
fn a_remote_that_could_carry_an_option_is_refused_before_anything_runs() {
    let (h, facts) = harness_and_facts();
    for url in [
        "git@example.invalid:-oProxyCommand=x",
        "ssh://-oProxyCommand=touch/x",
        "ext::sh:x",
        "git@example.invalid:org/../../x",
    ] {
        let done = h.run(&["up", url], "");
        assert_eq!(done.code, cli::exit::FAILED, "{url}: {}", done.all());
    }
    assert!(fetches(&facts).is_empty());
    assert!(!h.state().join("remote").exists() || std::fs::read_dir(h.state().join("remote")).unwrap().next().is_none());
}

#[test]
fn https_remotes_and_misplaced_flags_are_refused() {
    let (h, facts) = harness_and_facts();
    let done = h.run(&["up", "https://example.invalid/org/shop.git"], "");
    assert_eq!(done.code, cli::exit::FAILED, "{}", done.all());
    assert!(done.err.contains("only SSH"), "{}", done.err);
    let done = h.run(&["up", "--ref", "main"], "");
    assert_eq!(done.code, cli::exit::USAGE, "{}", done.all());
    let done = h.run(&["up", REMOTE, "--include-uncommitted"], "");
    assert_eq!(done.code, cli::exit::USAGE, "{}", done.all());
    assert!(fetches(&facts).is_empty());
}

#[test]
fn a_remote_never_fetched_is_not_found_by_other_commands() {
    let (h, facts) = harness_and_facts();
    let done = h.run(&["status", REMOTE], "");
    assert_eq!(done.code, cli::exit::FAILED, "{}", done.all());
    assert!(done.err.contains("not been fetched"), "{}", done.err);
    assert!(fetches(&facts).is_empty());
}

#[test]
fn only_an_agent_socket_of_this_user_is_passed_to_ssh() {
    let (h, facts) = harness_and_facts();
    let daemon = h.daemon.as_ref().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-socket");
    std::fs::write(&file, "").unwrap();
    let ask = |sock: &Path| {
        daemon.call(
            Request::Fetch(proto::FetchArgs {
                url: REMOTE.to_owned(),
                git_ref: None,
                fetch: true,
                ssh_auth_sock: Some(sock.to_str().unwrap().to_owned()),
            }),
            &mut |_, _| {},
        )
    };
    assert!(ask(&file).is_err());
    assert!(fetches(&facts).is_empty());

    let socket = dir.path().join("agent");
    let _agent = UnixListener::bind(&socket).unwrap();
    let report = match ask(&socket) {
        Ok(Reply::Fetch(report)) => report,
        other => panic!("{other:?}"),
    };
    assert!(report.fetched);
    assert_eq!(fetches(&facts)[0].2.as_deref(), Some(socket.as_path()));
}

#[test]
fn a_folder_whose_name_looks_like_a_remote_is_still_a_folder() {
    let (h, facts) = harness_and_facts();
    let odd = h.project.path().join("a:b");
    std::fs::create_dir(&odd).unwrap();
    let done = h.run(&["status", "a:b"], "");
    assert!(!done.err.contains("not been fetched"), "{}", done.all());
    assert!(fetches(&facts).is_empty());
}

// ---- Git through the relay: real Git on both sides

/// A remote with one commit on main, a project that was fetched from it, and
/// a folder that stands in for the sandbox's /work, where Git really runs.
struct RelayWorld {
    h: Harness,
    facts: Arc<Facts>,
    bare: PathBuf,
    work: PathBuf,
    _dirs: tempfile::TempDir,
}

fn relay_world() -> RelayWorld {
    let w = remote_setup();
    let up = w.h.run(&["up", REMOTE, "--all-ports"], "");
    assert_eq!(up.code, 0, "{}", up.all());
    w
}

/// The remote and the stand-in /work, before anything is started.
fn remote_setup() -> RelayWorld {
    let (h, facts) = harness_and_facts();
    let dirs = tempfile::tempdir().unwrap();
    let seed = dirs.path().join("seed");
    std::fs::create_dir(&seed).unwrap();
    test_git(&seed, &["init", "-q", "-b", "main"]);
    std::fs::write(seed.join("compose.yaml"), "services:\n  web:\n    image: web:1\n").unwrap();
    std::fs::write(seed.join("README"), "one\n").unwrap();
    test_git(&seed, &["add", "."]);
    test_git(&seed, &["commit", "-q", "-m", "first"]);
    test_git(&seed, &["branch", "release/1.2"]);
    let bare = dirs.path().join("origin.git");
    test_git(dirs.path(), &["clone", "-q", "--bare", seed.to_str().unwrap(), bare.to_str().unwrap()]);
    let work = dirs.path().join("work");
    std::fs::create_dir(&work).unwrap();
    *facts.git_remote.lock().unwrap() = Some(bare.clone());
    facts.engine.run_piped_in(&work);
    RelayWorld { h, facts, bare, work, _dirs: dirs }
}

#[test]
fn up_clones_the_remote_into_the_sandbox_with_its_history_and_no_credential() {
    let w = relay_world();
    assert_eq!(std::fs::read_to_string(w.work.join("README")).unwrap(), "one\n");
    assert_eq!(test_git(&w.work, &["log", "--format=%s"]).trim(), "first");
    assert_eq!(
        test_git(&w.work, &["rev-parse", "HEAD"]),
        test_git(&w.bare, &["rev-parse", "main"]),
        "the sandbox is at the approved commit"
    );
    assert_eq!(test_git(&w.work, &["remote", "get-url", "origin"]).trim(), REMOTE);
    // Only a fetch reached the remote, and nothing was copied in instead.
    assert_eq!(w.facts.relayed.lock().unwrap().iter().map(|r| r.0).collect::<Vec<_>>(), [gitbroker::GitService::UploadPack]);
    assert!(!w.facts.engine.calls().iter().any(|call| matches!(call, FakeCall::CopyIn(..))));

    // A push typed inside the sandbox has nothing to authenticate with.
    let plain = std::process::Command::new("git")
        .args(["push", "origin", "HEAD:refs/heads/sneaky"])
        .current_dir(&w.work)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &w.work)
        .output()
        .unwrap();
    assert!(!plain.status.success());
    let said = String::from_utf8_lossy(&plain.stderr);
    assert!(said.contains("holds no Git credentials"), "{said}");
    assert!(!test_git(&w.bare, &["branch", "--list"]).contains("sneaky"));
}

#[test]
fn a_branch_is_committed_and_pushed_through_the_relay() {
    let w = relay_world();
    test_git(&w.work, &["switch", "-q", "-c", "feature/one"]);
    std::fs::write(w.work.join("new.txt"), "made in the sandbox\n").unwrap();
    let commit = w.h.run(&["git", "commit", "-m", "Add new.txt", "--project", REMOTE], "");
    assert_eq!(commit.code, 0, "{}", commit.all());
    assert!(commit.out.contains("Add new.txt"), "{}", commit.out);

    let push = w.h.run(&["git", "push", "--project", REMOTE], "");
    assert_eq!(push.code, 0, "{}", push.all());
    assert!(push.out.contains("pushed refs/heads/feature/one"), "{}", push.out);
    assert_eq!(
        test_git(&w.bare, &["rev-parse", "refs/heads/feature/one"]),
        test_git(&w.work, &["rev-parse", "HEAD"])
    );
    assert_eq!(test_git(&w.bare, &["show", "feature/one:new.txt"]), "made in the sandbox\n");
    let log = std::fs::read_to_string(w.h.state().join("daemon-audit.jsonl")).unwrap_or_default();
    assert!(log.contains("git_pushed") && log.contains("refs/heads/feature/one"), "{log}");

    // The sandbox's branch now tracks the remote one.
    let status = w.h.run(&["git", "status", "--project", REMOTE], "");
    assert!(status.out.contains("origin/feature/one"), "{}", status.out);
}

#[test]
fn a_push_to_main_is_refused_before_the_remote_gets_anything() {
    let w = relay_world();
    let before = test_git(&w.bare, &["rev-parse", "main"]);
    std::fs::write(w.work.join("README"), "changed\n").unwrap();
    assert_eq!(w.h.run(&["git", "commit", "-m", "Change", "--project", REMOTE], "").code, 0);
    let push = w.h.run(&["git", "push", "--project", REMOTE], "");
    assert_eq!(push.code, cli::exit::FAILED, "{}", push.all());
    assert!(push.err.contains("refused") && push.err.contains("main"), "{}", push.all());
    assert!(push.all().contains("git switch -c"), "says what to do: {}", push.all());
    assert_eq!(test_git(&w.bare, &["rev-parse", "main"]), before, "main did not move");
    let log = std::fs::read_to_string(w.h.state().join("daemon-audit.jsonl")).unwrap_or_default();
    assert!(log.contains("git_push_refused"), "{log}");
}

#[test]
fn pull_brings_in_new_commits_without_recreating_anything() {
    let w = relay_world();
    // Someone else pushes to main.
    let other = w._dirs.path().join("other");
    test_git(w._dirs.path(), &["clone", "-q", w.bare.to_str().unwrap(), other.to_str().unwrap()]);
    std::fs::write(other.join("README"), "two\n").unwrap();
    test_git(&other, &["commit", "-q", "-am", "second"]);
    test_git(&other, &["push", "-q", "origin", "main"]);
    // The clone was made at a commit, so give main its upstream as a person would.
    test_git(&w.work, &["checkout", "-q", "-B", "main", "--track", "origin/main"]);

    let creates = w.facts.engine.calls().iter().filter(|c| matches!(c, FakeCall::Create(_))).count();
    let pull = w.h.run(&["git", "pull", "--project", REMOTE], "");
    assert_eq!(pull.code, 0, "{}", pull.all());
    assert_eq!(std::fs::read_to_string(w.work.join("README")).unwrap(), "two\n");
    let after = w.facts.engine.calls().iter().filter(|c| matches!(c, FakeCall::Create(_))).count();
    assert_eq!(creates, after, "no sandbox was made again");
}

/// The command line's `git --project REMOTE run ARGS...`.
fn git_run(w: &RelayWorld, args: &[&str]) -> common::Output {
    let mut all = vec!["git", "--project", REMOTE, "run"];
    all.extend_from_slice(args);
    w.h.run(&all, "")
}

#[test]
fn a_push_waits_for_the_person_and_names_the_branch_and_commits() {
    let w = relay_world();
    *w.facts.person.lock().unwrap() = Some(true);
    test_git(&w.work, &["switch", "-q", "-c", "feature/one"]);
    std::fs::write(w.work.join("new.txt"), "made in the sandbox\n").unwrap();
    assert_eq!(w.h.run(&["git", "commit", "-m", "Add", "--project", REMOTE], "").code, 0);
    let push = w.h.run(&["git", "push", "--project", REMOTE], "");
    assert_eq!(push.code, 0, "{}", push.all());
    let tip = test_git(&w.work, &["rev-parse", "HEAD"]);
    let asked = w.facts.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(asked[0].starts_with("push new branch feature/one at "), "{}", asked[0]);
    assert!(asked[0].contains(&tip[..7]) && asked[0].contains(REMOTE), "{}", asked[0]);
    assert!(push.all().contains("a test fingerprint"), "says it is waiting: {}", push.all());
    assert_eq!(test_git(&w.bare, &["rev-parse", "refs/heads/feature/one"]), tip);
}

#[test]
fn a_push_the_person_declines_sends_the_remote_nothing() {
    let w = relay_world();
    *w.facts.person.lock().unwrap() = Some(false);
    test_git(&w.work, &["switch", "-q", "-c", "feature/two"]);
    std::fs::write(w.work.join("new.txt"), "x\n").unwrap();
    assert_eq!(w.h.run(&["git", "commit", "-m", "Add", "--project", REMOTE], "").code, 0);
    let push = w.h.run(&["git", "push", "--project", REMOTE], "");
    assert_eq!(push.code, cli::exit::FAILED, "{}", push.all());
    assert!(push.all().contains("not confirmed"), "{}", push.all());
    let branches = test_git(&w.bare, &["branch", "--list", "feature/two"]);
    assert_eq!(branches, "", "the remote got the branch anyway");
    let log = std::fs::read_to_string(w.h.state().join("daemon-audit.jsonl")).unwrap_or_default();
    assert!(log.contains("person_declined"), "{log}");
}

#[test]
fn a_push_with_nothing_new_does_not_ask_the_person() {
    let w = relay_world();
    *w.facts.person.lock().unwrap() = Some(false);
    let before = test_git(&w.bare, &["for-each-ref"]);
    // main in the sandbox is where the remote's main is, so Git has nothing to send.
    let push = w.h.run(&["git", "push", "--project", REMOTE], "");
    assert_eq!(push.code, 0, "{}", push.all());
    assert!(w.facts.asked.lock().unwrap().is_empty(), "the person was asked for nothing");
    assert!(push.all().contains("Nothing needs pushing"), "{}", push.all());
    assert_eq!(test_git(&w.bare, &["for-each-ref"]), before);
}

#[test]
fn a_push_typed_as_a_git_command_is_asked_about_too() {
    let w = relay_world();
    *w.facts.person.lock().unwrap() = Some(false);
    test_git(&w.work, &["switch", "-q", "-c", "feature/three"]);
    std::fs::write(w.work.join("new.txt"), "x\n").unwrap();
    assert_eq!(w.h.run(&["git", "commit", "-m", "Add", "--project", REMOTE], "").code, 0);
    let push = git_run(&w, &["push", "origin", "feature/three"]);
    assert_ne!(push.code, 0, "{}", push.all());
    assert_eq!(w.facts.asked.lock().unwrap().len(), 1);
    assert_eq!(test_git(&w.bare, &["branch", "--list", "feature/three"]), "");
}

#[test]
fn a_pull_never_asks_the_person() {
    let w = relay_world();
    *w.facts.person.lock().unwrap() = Some(false);
    test_git(&w.work, &["checkout", "-q", "-B", "main", "--track", "origin/main"]);
    let pull = w.h.run(&["git", "pull", "--project", REMOTE], "");
    assert_eq!(pull.code, 0, "{}", pull.all());
    assert!(w.facts.asked.lock().unwrap().is_empty());
}


#[test]
fn typed_git_commands_run_in_the_sandbox_and_push_through_the_relay() {
    let w = relay_world();
    let switched = git_run(&w, &["switch", "-c", "typed/one"]);
    assert_eq!(switched.code, 0, "{}", switched.all());
    std::fs::write(w.work.join("typed.txt"), "typed\n").unwrap();
    assert_eq!(git_run(&w, &["add", "typed.txt"]).code, 0);
    let commit = git_run(&w, &["commit", "-m", "Typed commit"]);
    assert_eq!(commit.code, 0, "{}", commit.all());
    let log = git_run(&w, &["log", "--oneline", "-1"]);
    assert!(log.out.contains("Typed commit"), "{}", log.all());

    let push = git_run(&w, &["push", "-u", "origin", "HEAD"]);
    assert_eq!(push.code, 0, "{}", push.all());
    assert!(push.out.contains("pushed refs/heads/typed/one"), "{}", push.out);
    assert_eq!(test_git(&w.bare, &["show", "typed/one:typed.txt"]), "typed\n");
    // Only the push used the relay, as a push.
    let relayed: Vec<_> = w.facts.relayed.lock().unwrap().iter().map(|r| r.0).collect();
    assert_eq!(relayed, [gitbroker::GitService::UploadPack, gitbroker::GitService::ReceivePack]);

    let audit = std::fs::read_to_string(w.h.state().join("daemon-audit.jsonl")).unwrap_or_default();
    assert!(audit.contains("git_ran") && audit.contains("git_pushed"), "{audit}");
    assert!(!audit.contains("Typed commit"), "the audit log keeps the command name, not its arguments: {audit}");
}

#[test]
fn a_pull_over_local_changes_can_stash_and_reapply_them() {
    let w = relay_world();
    let other = w._dirs.path().join("other");
    test_git(w._dirs.path(), &["clone", "-q", w.bare.to_str().unwrap(), other.to_str().unwrap()]);
    // Someone else changes the same file as the sandbox, on another line.
    std::fs::write(other.join("compose.yaml"), "name: shop\nservices:\n  web:\n    image: web:1\n").unwrap();
    test_git(&other, &["commit", "-q", "-am", "second"]);
    test_git(&other, &["push", "-q", "origin", "main"]);
    test_git(&w.work, &["checkout", "-q", "-B", "main", "--track", "origin/main"]);
    std::fs::write(w.work.join("compose.yaml"), "services:\n  web:\n    image: web:2\n").unwrap();

    // The plain pull stops, as in the Changes tab.
    let pull = w.h.run(&["git", "pull", "--project", REMOTE], "");
    assert_eq!(pull.code, cli::exit::FAILED, "{}", pull.all());
    assert!(pull.all().contains("would be overwritten"), "{}", pull.all());

    let pull = git_run(&w, &["pull", "--no-rebase", "--no-edit", "--autostash"]);
    assert_eq!(pull.code, 0, "{}", pull.all());
    let compose = std::fs::read_to_string(w.work.join("compose.yaml")).unwrap();
    assert_eq!(compose, "name: shop\nservices:\n  web:\n    image: web:2\n", "their change came in and ours is back");
    assert!(test_git(&w.work, &["status", "--porcelain"]).contains("compose.yaml"), "ours is still uncommitted");
}

#[test]
fn typed_commands_cannot_get_around_the_relay_or_its_rules() {
    let w = relay_world();
    let main_before = test_git(&w.bare, &["rev-parse", "main"]);
    test_git(&w.work, &["switch", "-q", "-c", "keep"]);
    test_git(&w.work, &["push", "-q", w.bare.to_str().unwrap(), "keep"]);
    // Something new to push, so each refusal is a real one.
    std::fs::write(w.work.join("README"), "new\n").unwrap();
    test_git(&w.work, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-am", "new"]);

    for (args, says) in [
        (&["push", "origin", "HEAD:main"][..], "main"),
        (&["push", "origin", "--delete", "keep"][..], "deleting"),
        (&["push", "origin", "HEAD:refs/tags/v1"][..], "only branches"),
        (&["push", "-o", "ci.skip", "origin", "HEAD"][..], "options"),
        (&["-c", "core.sshCommand=sh", "fetch"][..], "Options before the command"),
        (&["fetch", "--upload-pack=sh"][..], "program run on the remote"),
    ] {
        let done = git_run(&w, args);
        assert_eq!(done.code, cli::exit::FAILED, "{args:?}: {}", done.all());
        assert!(done.all().contains(says), "{args:?}: {}", done.all());
    }
    assert_eq!(test_git(&w.bare, &["rev-parse", "main"]), main_before, "main did not move");
    assert!(test_git(&w.bare, &["branch", "--list", "keep"]).contains("keep"), "keep was not deleted");
    assert!(test_git(&w.bare, &["tag", "--list"]).trim().is_empty(), "no tag was pushed");

    // A command that is not fetch, pull or push gets no relay, so its own
    // attempt to reach the remote finds no credential.
    let update = git_run(&w, &["remote", "update"]);
    assert_eq!(update.code, cli::exit::FAILED, "{}", update.all());
    assert!(update.all().contains("holds no Git credentials"), "{}", update.all());

    // Nothing waits for an editor that is not there.
    std::fs::write(w.work.join("x.txt"), "x\n").unwrap();
    test_git(&w.work, &["add", "x.txt"]);
    let commit = git_run(&w, &["commit"]);
    assert_eq!(commit.code, cli::exit::FAILED, "{}", commit.all());
    assert!(commit.all().contains("no editor here"), "{}", commit.all());
}

#[test]
fn git_commands_refuse_a_project_that_did_not_come_from_a_remote() {
    let (h, _facts) = running();
    let done = h.run(&["git", "pull"], "");
    assert_eq!(done.code, cli::exit::FAILED, "{}", done.all());
    assert!(done.err.contains("not started from a Git remote"), "{}", done.err);
}

/// Like [`serve`], for a project named by its remote.
fn serve_remote(h: &Harness, calls: &[String]) -> Vec<serde_json::Value> {
    let mut input = mcp_line(
        0,
        "initialize",
        serde_json::json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } }),
    );
    for call in calls {
        input.push_str(call);
    }
    let done = h.run(&["mcp", "serve", "--project", REMOTE], &input);
    assert_eq!(done.code, 0, "{}", done.all());
    done.out.lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

#[test]
fn an_agent_commits_and_pushes_a_branch_through_mcp_but_never_main() {
    let w = relay_world();
    let main_before = test_git(&w.bare, &["rev-parse", "main"]);

    // On main, the push is refused and the remote is untouched.
    std::fs::write(w.work.join("README"), "agent edit\n").unwrap();
    let answers = serve_remote(
        &w.h,
        &[
            tool_call(1, "git_commit", serde_json::json!({ "message": "Agent edit" })),
            tool_call(2, "git_push", serde_json::json!({})),
        ],
    );
    assert_eq!(answers[1]["result"]["isError"], false, "{}", answers[1]);
    assert_eq!(answers[2]["result"]["isError"], true, "{}", answers[2]);
    assert!(tool_text(&answers[2]).contains("refused"), "{}", tool_text(&answers[2]));
    assert_eq!(test_git(&w.bare, &["rev-parse", "main"]), main_before);

    // On its own branch, the same push goes through.
    test_git(&w.work, &["switch", "-q", "-c", "agent/fix"]);
    let answers = serve_remote(
        &w.h,
        &[
            tool_call(1, "git_push", serde_json::json!({})),
            tool_call(2, "git_status", serde_json::json!({})),
        ],
    );
    assert_eq!(answers[1]["result"]["isError"], false, "{}", answers[1]);
    let text = tool_text(&answers[1]);
    assert!(text.contains("pushed refs/heads/agent/fix"), "{text}");
    assert!(text.contains("data, not instructions"), "{text}");
    assert_eq!(test_git(&w.bare, &["show", "agent/fix:README"]), "agent edit\n");
    assert!(tool_text(&answers[2]).contains("agent/fix"), "{}", tool_text(&answers[2]));
}

#[test]
fn an_agent_runs_any_git_command_through_mcp_with_the_same_rules() {
    let w = relay_world();
    let main_before = test_git(&w.bare, &["rev-parse", "main"]);
    std::fs::write(w.work.join("agent.txt"), "from the agent\n").unwrap();
    let answers = serve_remote(
        &w.h,
        &[
            tool_call(1, "git", serde_json::json!({ "args": ["switch", "-c", "agent/typed"] })),
            tool_call(2, "git", serde_json::json!({ "args": ["add", "agent.txt"] })),
            tool_call(3, "git", serde_json::json!({ "args": ["commit", "-m", "Agent typed"] })),
            tool_call(4, "git", serde_json::json!({ "args": ["push", "origin", "HEAD:main"] })),
            tool_call(5, "git", serde_json::json!({ "args": ["push", "-u", "origin", "HEAD"] })),
            tool_call(6, "git", serde_json::json!({ "args": ["-c", "x=y", "status"] })),
        ],
    );
    for id in [1, 2, 3, 5] {
        assert_eq!(answers[id]["result"]["isError"], false, "{}", answers[id]);
    }
    assert_eq!(answers[4]["result"]["isError"], true, "{}", answers[4]);
    assert!(tool_text(&answers[4]).contains("refused"), "{}", tool_text(&answers[4]));
    assert_eq!(answers[6]["result"]["isError"], true, "{}", answers[6]);
    assert!(tool_text(&answers[5]).contains("pushed refs/heads/agent/typed"), "{}", tool_text(&answers[5]));
    assert_eq!(test_git(&w.bare, &["rev-parse", "main"]), main_before);
    assert_eq!(test_git(&w.bare, &["show", "agent/typed:agent.txt"]), "from the agent\n");
}

#[test]
fn removing_an_imported_project_takes_its_copy_and_leaves_the_remote() {
    let w = relay_world();
    let before = test_git(&w.bare, &["rev-parse", "HEAD"]);
    let done = w.h.run(&["remove", REMOTE], "");
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(done.all().contains("The imported copy is gone"), "{}", done.all());
    assert_eq!(test_git(&w.bare, &["rev-parse", "HEAD"]), before);
    // Gone from the machine, so naming it again finds nothing.
    let again = w.h.run(&["remove", REMOTE], "");
    assert_ne!(again.code, 0, "{}", again.all());
}

// ---- new: the same requests as up, put to the person as one review

fn created(facts: &Facts) -> bool {
    facts.engine.calls().iter().any(|call| matches!(call, FakeCall::Create(_)))
}

/// Panics naming the first needle that is missing or out of place.
fn in_order(text: &str, needles: &[&str]) {
    let mut from = 0;
    for needle in needles {
        match text[from..].find(needle) {
            Some(at) => from += at + needle.len(),
            None => panic!("{needle:?} missing or out of order in:\n{text}"),
        }
    }
}

#[test]
fn new_on_a_folder_reviews_the_plan_then_starts_it_with_yes() {
    let (h, facts) = harness_and_facts();
    let done = h.run(&["new", ".", "--yes"], "");
    assert_eq!(done.code, 0, "{}", done.all());
    let text = done.out;
    println!("{text}");
    in_order(
        &text,
        &[
            "✓ Took ",
            "from its last commit (uncommitted changes stay out)",
            "✓ Read compose.yaml: 2 services (nothing has run yet)",
            "will be able to…",
            "  open       127.0.0.1:3000 → web    127.0.0.1:8080 → api",
            "  get        no secrets",
            "  use        4.0 GB memory · 2 CPUs · 30.0 GB disk",
            "plan ",
            "up ",
            "--plan-only shows all of it",
            "Starting with this plan, because of --yes.",
            "✓ Plan checked again",
            "✓ Virtual machine started",
            "✓ Last commit copied into the VM",
            "✓ Docker running inside the VM",
            "✓ No secrets to deliver",
            "· Building images and starting services",
            "✓ Images built and services started",
            "✓ Opened 127.0.0.1:3000 → web",
            "✓ Opened 127.0.0.1:8080 → api",
            "What runs inside the sandbox is untrusted",
            "Next: ",
            "logs --project ",
        ],
    );
    assert!(created(&facts));
}

#[test]
fn new_groups_the_review_as_reach_open_get_use_then_warnings_then_the_digest() {
    let (h, _facts) = harness_and_facts();
    let text = h.run(&["new", "."], "").out;
    let name = h.project.path().canonicalize().unwrap();
    let name = name.file_name().unwrap().to_str().unwrap().to_owned();
    in_order(
        &text,
        &[
            &format!("{name} will be able to…"),
            "\n  reach      ",
            "\n  open       ",
            "\n  get        ",
            "\n  use        ",
            "\n! ",
            "\nplan ",
        ],
    );
    // Only words the plan holds: build hosts stay open, as up says too.
    assert!(text.contains("build hosts stay reachable after the build"), "{text}");
    assert!(text.contains("while building, and after"), "{text}");
}

#[test]
fn new_says_which_service_gets_each_secret_and_where_it_could_go() {
    let (h, _facts) = harness_and_facts();
    h.answer(&["sk-test-value", "sk-test-value"]);
    let set = h.run(&["secrets", "set", "API_KEY", "--service", "api"], "");
    assert_eq!(set.code, 0, "{}", set.all());
    let text = h.run(&["new", "."], "").out;
    println!("{text}");
    in_order(
        &text,
        &[
            "  get        API_KEY → api (in the environment)",
            "  use        ",
            "! API_KEY is given to api",
            "! Code in those services could send them to any of these",
            "\n  auth.docker.io,",
            "\nplan ",
        ],
    );
    assert!(!text.contains("sk-test-value"), "{text}");
}

#[test]
fn new_starts_nothing_when_the_answer_is_the_default() {
    for answer in ["\n", "n\n", "", "sure\n"] {
        let (mut h, facts) = harness_and_facts();
        h.terminal = true;
        let done = h.run(&["new", "."], answer);
        assert_eq!(done.code, cli::exit::FAILED, "{answer:?}: {}", done.all());
        assert!(done.out.contains("Start with this plan? [y/N]"), "{}", done.out);
        assert!(done.out.contains("Nothing was started."), "{}", done.out);
        assert!(!created(&facts), "{answer:?} started something");
    }
}

#[test]
fn new_starts_on_a_terminal_when_the_answer_is_yes() {
    let (mut h, facts) = harness_and_facts();
    h.terminal = true;
    let done = h.run(&["new", "."], "y\n");
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(done.out.contains("✓ Images built and services started"), "{}", done.out);
    assert!(created(&facts));
}

#[test]
fn new_without_a_terminal_prints_the_review_and_refuses_without_yes() {
    let (h, facts) = harness_and_facts();
    // An answer on stdin is not a person at a terminal.
    let done = h.run(&["new", "."], "y\n");
    assert_eq!(done.code, cli::exit::FAILED, "{}", done.all());
    assert!(done.out.contains("will be able to…"), "{}", done.out);
    assert!(done.out.contains("  open       "), "{}", done.out);
    assert!(!done.out.contains("[y/N]"), "{}", done.out);
    assert!(done.err.contains("needs --yes"), "{}", done.err);
    assert!(!created(&facts));
}

#[test]
fn new_opens_only_the_ports_asked_for_and_refuses_one_not_in_the_plan() {
    let (h, facts) = harness_and_facts();
    let done = h.run(&["new", ".", "--port", "5432", "--yes"], "");
    assert_eq!(done.code, cli::exit::USAGE, "{}", done.all());
    assert!(done.err.contains("does not publish 5432"), "{}", done.err);
    assert!(!created(&facts));

    let done = h.run(&["new", ".", "--port", "3000", "--yes"], "");
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(done.out.contains("not opened (not chosen with --port): 127.0.0.1:8080 → api"), "{}", done.out);
    assert!(done.out.contains("✓ Opened 127.0.0.1:3000 → web"), "{}", done.out);
    assert!(!done.out.contains("✓ Opened 127.0.0.1:8080"), "{}", done.out);
}

#[test]
fn new_stops_at_a_blocker_like_up() {
    let (h, facts) = harness_and_facts();
    std::fs::write(h.project.path().join(brand::CONFIG_FILE), "resources:\n  memory: 2G\n").unwrap();
    let done = h.run(&["new", ".", "--yes"], "");
    assert_eq!(done.code, cli::exit::FAILED, "{}", done.all());
    assert!(done.out.contains("will be able to…"), "{}", done.out);
    assert!(done.out.contains("This cannot be started yet"), "{}", done.out);
    assert!(!created(&facts));
}

#[test]
fn new_with_a_remote_fetches_reviews_and_clones_through_the_relay() {
    let mut w = remote_setup();
    w.h.terminal = true;
    let head = test_git(&w.bare, &["rev-parse", "HEAD"]).trim().to_owned();
    let done = w.h.run(&["new", REMOTE], "y\n");
    assert_eq!(done.code, 0, "{}", done.all());
    let text = done.out;
    println!("{text}");
    in_order(
        &text,
        &[
            &format!("· Fetching {REMOTE} on this machine, with your SSH key"),
            &format!("✓ Fetched the default branch @ {} with your SSH key (the key stays here)", &head[..7]),
            "✓ Read compose.yaml: 1 service (nothing has run yet)",
            "shop will be able to…",
            &format!("up {REMOTE} --plan-only shows all of it, after fetching again"),
            "✓ Cloned into the VM through the relay, with no key inside",
            "✓ Images built and services started",
            &format!("logs --project {REMOTE}"),
        ],
    );
    assert_eq!(fetches(&w.facts), [(REMOTE.to_owned(), "HEAD".to_owned(), None)]);
    assert_eq!(test_git(&w.work, &["rev-parse", "HEAD"]).trim(), head, "the sandbox was cloned at that commit");
}

#[test]
fn new_with_a_remote_passes_on_the_ref_and_a_folder_refuses_one() {
    let w = remote_setup();
    let done = w.h.run(&["new", REMOTE, "--ref", "release/1.2", "--yes"], "");
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(done.out.contains("✓ Fetched release/1.2 @ "), "{}", done.out);
    assert_eq!(fetches(&w.facts)[0].1, "release/1.2");

    let (h, facts) = harness_and_facts();
    let done = h.run(&["new", ".", "--ref", "main"], "");
    assert_eq!(done.code, cli::exit::USAGE, "{}", done.all());
    assert!(!created(&facts));
}

#[test]
fn new_with_a_remote_and_no_terminal_fetches_and_reviews_but_starts_nothing() {
    let w = remote_setup();
    let done = w.h.run(&["new", REMOTE], "");
    assert_eq!(done.code, cli::exit::FAILED, "{}", done.all());
    assert!(done.out.contains("✓ Fetched"), "{}", done.out);
    assert!(done.out.contains("shop will be able to…"), "{}", done.out);
    assert!(done.err.contains("needs --yes"), "{}", done.err);
    assert!(!created(&w.facts));
    assert!(w.facts.relayed.lock().unwrap().is_empty(), "nothing was cloned");
    // The fetched copy is on this machine all the same, and remove takes it.
    let removed = w.h.run(&["remove", REMOTE], "");
    assert_eq!(removed.code, 0, "{}", removed.all());
    assert!(removed.out.contains("The imported copy is gone"), "{}", removed.out);
}

#[test]
fn net_allow_replaces_the_sandbox_and_says_the_data_was_kept() {
    let w = relay_world();
    let done = w.h.run(&["net", "allow", "api.example.com", "--project", REMOTE], "");
    assert_eq!(done.code, 0, "{}", done.all());
    assert!(done.all().contains("api.example.com is allowed now"), "{}", done.all());
    assert!(done.all().contains("with its data kept"), "{}", done.all());
    let again = w.h.run(&["net", "allow", "api.example.com", "--project", REMOTE], "");
    assert!(again.all().contains("already allowed"), "{}", again.all());
    let bad = w.h.run(&["net", "allow", "*.example.com", "--project", REMOTE], "");
    assert_ne!(bad.code, 0, "{}", bad.all());
}
