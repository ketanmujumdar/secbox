//! Comparing a sandbox's `/work` with the snapshot it was given, against a
//! fake engine.
//!
//! The fake's streamed exec plays the guest: each test builds the bundle a
//! guest would, with ordinary git in a temporary folder, from the host's own
//! copy of the baseline, and hands it over as base64. None of this shows the
//! guest script works on a real VM; `docs/NEEDS_A_VM.md` says so. What it
//! shows is what the host does with whatever comes back.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use daemon::{Deps, Quiet, Service};
use engine::{Engine, FakeCall, FakeEngine, OutputStream};
use hoststate::host::{HostInfo, Hypervisor};
use hoststate::StatePaths;
use proto::{
    ErrorCode, FileChange, PlanArgs, ProjectArgs, Reply, Request, SnapshotCounts, SnapshotPlan,
    Source, UpArgs,
};
use secrets::{MemoryStore, ProjectId, SecretStore};

const GIB: u64 = 1 << 30;

struct TestDeps {
    engine: FakeEngine,
    /// The snapshot arrives with something at `.git` already, so the
    /// baseline cannot be made.
    snapshot_has_git: bool,
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

    /// The project's top-level files, as a commit snapshot of them would be.
    fn snapshot(&self, root: &Path, _source: &Source, dest: &Path) -> Result<SnapshotPlan, String> {
        std::fs::create_dir(dest).expect("snapshot destination must not exist yet");
        for entry in std::fs::read_dir(root).unwrap().flatten() {
            if entry.file_type().unwrap().is_file() {
                std::fs::copy(entry.path(), dest.join(entry.file_name())).unwrap();
            }
        }
        if self.snapshot_has_git {
            std::fs::write(dest.join(".git"), "gitdir: /elsewhere\n").unwrap();
        }
        Ok(SnapshotPlan {
            revision: Some("HEAD".to_owned()),
            counts: Some(SnapshotCounts {
                files: 2,
                total_bytes: 64,
            }),
            excluded: Vec::new(),
            warnings: Vec::new(),
        })
    }
}

struct Fixture {
    state: tempfile::TempDir,
    project: tempfile::TempDir,
    scratch: tempfile::TempDir,
    service: Service,
    deps: Arc<TestDeps>,
}

const COMPOSE: &str = "services:\n  api:\n    image: api:1\n";

fn fixture() -> Fixture {
    fixture_with(false)
}

fn fixture_with(snapshot_has_git: bool) -> Fixture {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("compose.yaml"), COMPOSE).unwrap();
    std::fs::write(project.path().join("app.js"), "console.log('hello');\n").unwrap();
    let state = tempfile::tempdir().unwrap();
    let deps = Arc::new(TestDeps {
        engine: FakeEngine::new(),
        snapshot_has_git,
    });
    let paths = StatePaths {
        state: state.path().join(brand::STATE_DIR),
        overridden: true,
    };
    let service = Service::new(paths, Arc::clone(&deps) as Arc<dyn Deps>).unwrap();
    Fixture {
        state,
        project,
        scratch: tempfile::tempdir().unwrap(),
        service,
        deps,
    }
}

impl Fixture {
    fn path(&self) -> String {
        self.project.path().to_string_lossy().into_owned()
    }

    fn call(&self, request: Request) -> Result<Reply, proto::ApiError> {
        self.service.handle(&request, &mut Quiet)
    }

    fn up(&self) {
        let digest = match self
            .call(Request::Plan(PlanArgs {
                project: self.path(),
                source: Source::default(),
            }))
            .unwrap()
        {
            Reply::Plan(plan) => plan.digest,
            other => panic!("unexpected {other:?}"),
        };
        self.call(Request::Up(UpArgs {
            project: self.path(),
            source: Source::default(),
            plan_digest: digest,
            approve_ports: Vec::new(),
        }))
        .unwrap();
    }

    fn diff(&self) -> Result<proto::DiffReport, proto::ApiError> {
        match self.call(Request::Diff(ProjectArgs {
            project: self.path(),
        }))? {
            Reply::Diff(report) => Ok(report),
            other => panic!("unexpected {other:?}"),
        }
    }

    /// The project's folder in the state folder, found by its baseline.
    fn project_state(&self) -> PathBuf {
        fn find(dir: &Path) -> Option<PathBuf> {
            for entry in std::fs::read_dir(dir).ok()?.flatten() {
                let path = entry.path();
                if entry.file_name() == daemon::diff::BASELINE_STORE {
                    return Some(dir.to_path_buf());
                }
                if entry.file_type().ok()?.is_dir() {
                    if let Some(found) = find(&path) {
                        return Some(found);
                    }
                }
            }
            None
        }
        find(self.state.path()).expect("no baseline was kept")
    }

    fn baseline(&self) -> String {
        let record: serde_json::Value = serde_json::from_slice(
            &std::fs::read(self.project_state().join("sandbox.json")).unwrap(),
        )
        .unwrap();
        record["baseline"]
            .as_str()
            .expect("no baseline recorded")
            .to_owned()
    }

    /// Plays the guest: checks out the baseline from the host's copy, lets
    /// `change` edit it, commits, and bundles the result as the guest script
    /// would. Returns the bundle's bytes.
    fn guest_bundle(&self, change: impl FnOnce(&Path)) -> Vec<u8> {
        let base = self.baseline();
        let store = self.project_state().join(daemon::diff::BASELINE_STORE);
        let work = self.scratch.path().join("work");
        git(
            self.scratch.path(),
            &["init", "-q", "-b", "main", work.to_str().unwrap()],
        );
        git(
            &work,
            &[
                "fetch",
                "-q",
                store.to_str().unwrap(),
                &format!("refs/base/{base}:refs/heads/base"),
            ],
        );
        git(&work, &["checkout", "-q", "-B", "main", "base"]);
        change(&work);
        git(&work, &["add", "--all", "--force"]);
        git(
            &work,
            &["commit", "-q", "--allow-empty", "--no-verify", "-m", "work"],
        );
        git(&work, &["update-ref", daemon::diff::TIP_REF, "HEAD"]);
        let out = self.scratch.path().join("out.bundle");
        git(
            &work,
            &[
                "bundle",
                "create",
                "-q",
                out.to_str().unwrap(),
                daemon::diff::TIP_REF,
                &format!("^{base}"),
            ],
        );
        std::fs::read(out).unwrap()
    }

    /// Queues what the guest's command would print.
    fn guest_sends(&self, stdout: &[u8]) {
        for piece in stdout.chunks(1000) {
            self.deps
                .engine
                .push_stream_output(OutputStream::Stdout, piece);
        }
        self.deps.engine.set_stream_code(0);
    }

    fn calls_since(&self, start: usize) -> Vec<FakeCall> {
        self.deps.engine.calls()[start..].to_vec()
    }
}

/// Ordinary git, for building what a guest would send. Never the user's config.
fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "guest")
        .env("GIT_AUTHOR_EMAIL", "guest@example.invalid")
        .env("GIT_COMMITTER_NAME", "guest")
        .env("GIT_COMMITTER_EMAIL", "guest@example.invalid")
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Base64 as the guest's `base64` prints it, in lines of 76.
fn base64(bytes: &[u8]) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = Vec::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                text.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]);
            } else {
                text.push(b'=');
            }
        }
    }
    let mut out = Vec::new();
    for line in text.chunks(76) {
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    out
}

const PLANTED_HOOK: &str = r#"{"hooks":{"SessionStart":[{"command":"touch MARKER"}]}}"#;

#[test]
fn starting_a_sandbox_keeps_a_baseline_and_gives_the_guest_the_same_commit() {
    let f = fixture();
    f.up();
    let base = f.baseline();
    assert_eq!(base.len(), 40);
    let sandbox = daemon_sandbox(&f);
    // The guest's /work is a repository at the host's commit.
    let head = f
        .deps
        .engine
        .copied_file(&sandbox, "/work/.git/refs/heads/main");
    assert_eq!(
        head.map(|h| String::from_utf8(h).unwrap().trim().to_owned()),
        Some(base)
    );
    assert!(f
        .deps
        .engine
        .copied_file(&sandbox, "/work/app.js")
        .is_some());
}

fn daemon_sandbox(f: &Fixture) -> engine::SandboxName {
    f.deps
        .engine
        .calls()
        .iter()
        .find_map(|call| match call {
            FakeCall::Create(spec) => Some(spec.name.clone()),
            _ => None,
        })
        .expect("a sandbox was created")
}

#[test]
fn a_planted_ai_tool_hook_is_flagged_first_and_nothing_from_the_guest_runs() {
    let f = fixture();
    f.up();
    let marker = f.scratch.path().join("ran");
    let hook = PLANTED_HOOK.replace("MARKER", marker.to_str().unwrap());
    let bundle = f.guest_bundle(|work| {
        std::fs::create_dir_all(work.join(".claude")).unwrap();
        std::fs::write(work.join(".claude/settings.json"), &hook).unwrap();
        std::fs::write(
            work.join("package.json"),
            format!(
                r#"{{"scripts":{{"postinstall":"touch {}"}}}}"#,
                marker.display()
            ),
        )
        .unwrap();
        std::fs::create_dir_all(work.join(".githooks")).unwrap();
        std::fs::write(
            work.join(".githooks/post-checkout"),
            format!("#!/bin/sh\ntouch {}\n", marker.display()),
        )
        .unwrap();
        std::fs::write(work.join("app.js"), "console.log('changed');\n").unwrap();
    });
    f.guest_sends(&base64(&bundle));
    let before = f.deps.engine.calls().len();

    let report = f.diff().unwrap();
    assert!(report.flagged());
    assert!(report.complete);
    let first = &report.flags[0];
    assert_eq!(first.kind, "agent_hook");
    assert_eq!(first.path.as_deref(), Some(".claude/settings.json"));
    assert!(
        first.message.contains("would run on your machine"),
        "{}",
        first.message
    );
    for kind in ["claude_config", "install_script", "git_hooks"] {
        assert!(
            report.flags.iter().any(|f| f.kind == kind),
            "{kind}: {:?}",
            report.flags
        );
    }
    let changed: Vec<(&str, FileChange)> = report
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.change))
        .collect();
    assert!(
        changed.contains(&(".claude/settings.json", FileChange::Added)),
        "{changed:?}"
    );
    assert!(
        changed.contains(&("app.js", FileChange::Modified)),
        "{changed:?}"
    );
    assert_eq!(report.baseline, f.baseline());

    // The guest was asked for one thing, in /work, and nothing else ran there.
    let calls = f.calls_since(before);
    let streams: Vec<_> = calls
        .iter()
        .filter_map(|c| match c {
            FakeCall::ExecStream(_, request) => Some(request),
            _ => None,
        })
        .collect();
    assert_eq!(streams.len(), 1, "{calls:?}");
    assert_eq!(
        streams[0].workdir.as_ref().map(|w| w.as_str()),
        Some("/work")
    );
    assert_eq!(streams[0].command.last(), Some(&report.baseline));
    assert!(
        calls
            .iter()
            .all(|c| matches!(c, FakeCall::Status(_) | FakeCall::ExecStream(..))),
        "{calls:?}"
    );
    // Every hook, script and hook-shaped file stayed data.
    assert!(
        !marker.exists(),
        "something from the sandbox ran on the host"
    );
}

#[test]
fn a_clean_change_is_listed_and_not_flagged() {
    let f = fixture();
    f.up();
    let bundle = f.guest_bundle(|work| {
        std::fs::write(work.join("app.js"), "console.log('changed');\n").unwrap();
        std::fs::create_dir_all(work.join("src")).unwrap();
        std::fs::write(work.join("src/util.js"), "export const x = 1;\n").unwrap();
    });
    f.guest_sends(&base64(&bundle));
    let report = f.diff().unwrap();
    assert!(!report.flagged(), "{:?}", report.flags);
    let changed: Vec<(&str, FileChange)> = report
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.change))
        .collect();
    assert_eq!(
        changed,
        [
            ("app.js", FileChange::Modified),
            ("src/util.js", FileChange::Added)
        ]
    );
}

#[test]
fn nothing_changed_is_an_empty_report() {
    let f = fixture();
    f.up();
    let bundle = f.guest_bundle(|_| {});
    f.guest_sends(&base64(&bundle));
    let report = f.diff().unwrap();
    assert!(report.files.is_empty());
    assert!(!report.flagged());
}

#[test]
fn a_hostile_name_is_escaped_and_a_case_trick_is_still_flagged() {
    let f = fixture();
    f.up();
    let bundle = f.guest_bundle(|work| {
        std::fs::create_dir_all(work.join(".CLAUDE")).unwrap();
        std::fs::write(work.join(".CLAUDE/Settings.json"), PLANTED_HOOK).unwrap();
        std::fs::write(work.join("evil\u{1b}[31mname\u{202e}.txt"), "x").unwrap();
    });
    f.guest_sends(&base64(&bundle));
    let report = f.diff().unwrap();
    assert_eq!(report.flags[0].kind, "agent_hook");
    let all = serde_json::to_string(&report).unwrap();
    assert!(
        !all.contains('\u{1b}') && !all.contains('\u{202e}'),
        "{all}"
    );
    assert!(report.flags.iter().any(|f| f.kind == "suspicious_path"));
}

#[test]
fn a_sandbox_that_rewrote_its_history_is_still_compared_with_the_host_baseline() {
    let f = fixture();
    f.up();
    // A guest that throws the baseline away and sends a history of its own.
    let work = f.scratch.path().join("unrelated");
    git(
        f.scratch.path(),
        &["init", "-q", "-b", "main", work.to_str().unwrap()],
    );
    std::fs::write(work.join("compose.yaml"), COMPOSE).unwrap();
    git(&work, &["add", "--all"]);
    git(&work, &["commit", "-q", "-m", "mine"]);
    git(&work, &["update-ref", daemon::diff::TIP_REF, "HEAD"]);
    let out = f.scratch.path().join("u.bundle");
    git(
        &work,
        &[
            "bundle",
            "create",
            "-q",
            out.to_str().unwrap(),
            daemon::diff::TIP_REF,
        ],
    );
    f.guest_sends(&base64(&std::fs::read(out).unwrap()));
    let report = f.diff().unwrap();
    assert!(
        report.flags.iter().any(|f| f.kind == "base_not_ancestor"),
        "{:?}",
        report.flags
    );
    // app.js is in the host's baseline and not in what was sent.
    assert!(report
        .files
        .iter()
        .any(|f| f.path == "app.js" && f.change == FileChange::Deleted));
}

fn refused_with(f: &Fixture, stdout: &[u8]) -> proto::ApiError {
    f.guest_sends(stdout);
    f.diff().expect_err("a hostile bundle must be refused")
}

#[test]
fn hostile_output_is_refused_safely() {
    let f = fixture();
    f.up();
    let real = f.guest_bundle(|work| std::fs::write(work.join("a.txt"), "a").unwrap());

    // Not base64, such as terminal codes or masked text.
    let error = refused_with(&f, b"\x1b[31mhello\n");
    assert_eq!(error.code, ErrorCode::Refused);
    assert!(!error.message.contains('\u{1b}'));
    // Valid base64 of something that is not a bundle.
    assert_eq!(
        refused_with(&f, &base64(b"#!/bin/sh\nrm -rf /\n")).code,
        ErrorCode::Refused
    );
    // A real bundle cut short.
    assert_eq!(
        refused_with(&f, &base64(&real[..real.len() / 2])).code,
        ErrorCode::Refused
    );
    // A bundle cut part way through a base64 group.
    let mut cut = base64(&real);
    cut.truncate(cut.len() - 3);
    assert_eq!(refused_with(&f, &cut).code, ErrorCode::Refused);
    // Nothing at all.
    assert_eq!(refused_with(&f, b"").code, ErrorCode::Refused);
    // A header naming a ref that could be read as an option.
    let header = format!(
        "# v2 git bundle\n{} refs/heads/-evil\n\nPACK",
        "a".repeat(40)
    );
    assert_eq!(
        refused_with(&f, &base64(header.as_bytes())).code,
        ErrorCode::Refused
    );
}

#[test]
fn a_bundle_with_another_branch_or_more_than_one_is_refused() {
    let f = fixture();
    f.up();
    let base = f.baseline();
    let _ = f.guest_bundle(|work| std::fs::write(work.join("a.txt"), "a").unwrap());
    let work = f.scratch.path().join("work");
    let out = f.scratch.path().join("two.bundle");
    git(
        &work,
        &[
            "bundle",
            "create",
            "-q",
            out.to_str().unwrap(),
            daemon::diff::TIP_REF,
            "refs/heads/main",
            &format!("^{base}"),
        ],
    );
    let error = refused_with(&f, &base64(&std::fs::read(&out).unwrap()));
    assert!(
        error.message.contains("exactly the one branch"),
        "{}",
        error.message
    );

    let other = f.scratch.path().join("other.bundle");
    git(
        &work,
        &[
            "bundle",
            "create",
            "-q",
            other.to_str().unwrap(),
            "refs/heads/main",
            &format!("^{base}"),
        ],
    );
    let error = refused_with(&f, &base64(&std::fs::read(&other).unwrap()));
    assert!(
        error.message.contains("exactly the one branch"),
        "{}",
        error.message
    );
}

#[test]
fn a_failed_guest_command_says_why_escaped() {
    let f = fixture();
    f.up();
    f.deps
        .engine
        .push_stream_output(OutputStream::Stderr, b"git: not found \x1b[2J\n");
    f.deps.engine.set_stream_code(127);
    let error = f.diff().unwrap_err();
    assert_eq!(error.code, ErrorCode::Engine);
    assert!(
        error.message.contains("git: not found"),
        "{}",
        error.message
    );
    assert!(!error.message.contains('\u{1b}'));
}

#[test]
fn no_sandbox_or_a_stopped_one_is_refused_without_running_anything() {
    let f = fixture();
    let error = f.diff().unwrap_err();
    assert_eq!(error.code, ErrorCode::WrongState);
    f.up();
    f.call(Request::Down(proto::DownArgs {
        project: f.path(),
        destroy: false,
    }))
    .unwrap();
    let before = f.deps.engine.calls().len();
    let error = f.diff().unwrap_err();
    assert_eq!(error.code, ErrorCode::WrongState);
    assert!(f
        .calls_since(before)
        .iter()
        .all(|c| matches!(c, FakeCall::Status(_))));
}

#[test]
fn a_sandbox_with_no_baseline_on_record_is_refused_with_the_way_out() {
    let f = fixture();
    f.up();
    let record_path = f.project_state().join("sandbox.json");
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
    record.as_object_mut().unwrap().remove("baseline");
    std::fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
    let before = f.deps.engine.calls().len();
    let error = f.diff().unwrap_err();
    assert_eq!(error.code, ErrorCode::WrongState);
    assert!(
        error.message.contains("down --destroy"),
        "{}",
        error.message
    );
    assert!(!f
        .calls_since(before)
        .iter()
        .any(|c| matches!(c, FakeCall::ExecStream(..))));
}

#[test]
fn destroying_the_sandbox_removes_the_hosts_copy_of_the_baseline() {
    let f = fixture();
    f.up();
    let store = f.project_state().join(daemon::diff::BASELINE_STORE);
    assert!(store.is_dir());
    f.call(Request::Down(proto::DownArgs {
        project: f.path(),
        destroy: true,
    }))
    .unwrap();
    assert!(!store.exists());
}

/// A start does not fail because the comparison cannot be prepared, but the
/// comparison then says why it cannot run, and what was given is untouched.
#[test]
fn a_baseline_that_cannot_be_made_does_not_stop_the_start() {
    let f = fixture_with(true);
    f.up();
    let sandbox = daemon_sandbox(&f);
    assert_eq!(
        f.deps.engine.copied_file(&sandbox, "/work/.git").as_deref(),
        Some(&b"gitdir: /elsewhere\n"[..])
    );
    let audit = std::fs::read_to_string(f.service.audit().path()).unwrap();
    assert!(audit.contains("baseline_missing"), "{audit}");
    let error = f.diff().unwrap_err();
    assert_eq!(error.code, ErrorCode::WrongState);
}
