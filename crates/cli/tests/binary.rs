//! Runs the real binary. Nothing here reaches the secret store.

mod common;

use std::fs;
use std::os::unix::fs::symlink;

use cli::exit;
use common::{fixture, run_binary, run_binary_with, NOT_BUILT};
use tempfile::TempDir;

fn home() -> TempDir {
    TempDir::new().unwrap()
}

fn fixture_arg(name: &str) -> String {
    fixture(name).to_string_lossy().into_owned()
}

#[test]
fn help_and_usage_use_the_brand_name() {
    let dir = home();
    let help = run_binary(dir.path(), &["--help"]);
    assert_eq!(help.code, exit::OK);
    assert!(help.out.contains(&format!("Usage: {}", brand::CLI_BIN)));
    assert!(help.out.contains("Do not rely on it"));
    for name in [
        "doctor",
        "check",
        "resources",
        "secrets",
        "net",
        "uninstall",
    ] {
        assert!(help.out.contains(name), "{name}");
    }
    for name in ["up", "down", "status", "diff", "export"] {
        assert!(help.out.contains(&format!("  {name} ")), "{name}");
    }
    let sub = run_binary(dir.path(), &["secrets", "--help"]);
    assert!(sub.out.contains(&format!("{} secrets", brand::CLI_BIN)));
    let version = run_binary(dir.path(), &["--version"]);
    assert!(version.out.starts_with(brand::CLI_BIN));
}

#[test]
fn unbuilt_commands_refuse_with_a_distinct_code() {
    let dir = home();
    for command in ["diff", "export"] {
        let done = run_binary(dir.path(), &[command]);
        assert_eq!(done.code, NOT_BUILT, "{command}");
        assert!(done.err.contains("not built yet"), "{command}");
        assert!(done.err.contains("Do not rely on it"), "{command}");
        assert!(!dir.path().join(brand::STATE_DIR).exists());
    }
    assert_ne!(NOT_BUILT, exit::OK);
    assert_ne!(NOT_BUILT, exit::FAILED);
    assert_ne!(NOT_BUILT, exit::USAGE);
}

/// The commands that run a sandbox are built, but they do nothing on their
/// own: they ask the daemon. With no daemon they must say so plainly, and
/// they must not fall back to doing the work themselves.
#[test]
fn the_sandbox_commands_report_a_daemon_that_is_not_running() {
    let dir = home();
    for command in ["up", "down", "status"] {
        let done = run_binary(dir.path(), &[command]);
        assert_eq!(done.code, exit::FAILED, "{command}: {}", done.all());
        assert!(
            done.err.contains(brand::DAEMON_BIN),
            "{command} should name the daemon: {}",
            done.all()
        );
        assert!(
            done.err.contains("not running"),
            "{command}: {}",
            done.all()
        );
    }
}

#[test]
fn doctor_runs_without_path_docker_or_writing_anything() {
    let dir = home();
    // The environment is cleared, so there is no PATH, and so no Docker.
    let done = run_binary(dir.path(), &["doctor"]);
    assert!(done.code == exit::OK || done.code == exit::FAILED);
    for label in [
        "Platform",
        "Hypervisor",
        "CPU",
        "Memory",
        "Disk",
        "State folder",
        "Socket path",
        "Engine",
    ] {
        assert!(done.out.contains(label), "{label}: {}", done.out);
    }
    assert!(done.out.contains("Do not rely on it"));
    assert!(!done.out.to_lowercase().contains("docker"));
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn doctor_fails_on_a_socket_path_that_is_too_long() {
    let dir = home();
    let long = dir.path().join("x".repeat(90));
    fs::create_dir(&long).unwrap();
    let done = run_binary_with(
        dir.path(),
        &["doctor"],
        &[(&common::home_var(), long.to_str().unwrap())],
        "",
    );
    assert_eq!(done.code, exit::FAILED, "{}", done.out);
    assert!(done.out.contains("Socket path"));
    assert!(done.out.contains("next  use a shorter home folder"));
}

#[test]
fn doctor_fails_on_a_symlink_at_the_state_path() {
    let dir = home();
    let elsewhere = home();
    symlink(elsewhere.path(), dir.path().join(brand::STATE_DIR)).unwrap();
    let done = run_binary(dir.path(), &["doctor"]);
    assert_eq!(done.code, exit::FAILED);
    assert!(done.out.contains("symbolic link"));
}

#[test]
fn doctor_never_runs_a_binary_planted_in_the_current_folder() {
    let dir = home();
    let planted = dir.path().join(".tools/msb/bin");
    fs::create_dir_all(&planted).unwrap();
    let canary = dir.path().join("ran");
    let script = planted.join("msb");
    fs::write(&script, format!("#!/bin/sh\ntouch {}\n", canary.display())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let done = run_binary(dir.path(), &["doctor"]);
    assert!(done.out.contains("not found"), "{}", done.out);
    assert!(!canary.exists());
}

#[test]
fn check_groups_a_hostile_compose_file_and_escapes_it() {
    let dir = home();
    let done = run_binary(dir.path(), &["check", &fixture_arg("risky")]);
    assert_eq!(done.code, exit::OK, "{}", done.all());
    let text = done.all();
    let critical = text.find("Critical (").expect("critical group");
    let high = text.find("High (").expect("high group");
    assert!(critical < high);
    assert!(text.contains("web"));
    assert!(text.contains("Do not rely on it"));
    assert!(!text.contains('\u{1b}'), "raw escape: {text:?}");
    assert!(!text.contains('\u{7}'), "raw bell: {text:?}");
    assert!(!text.contains('\u{202e}'), "raw bidi: {text:?}");
    assert!(
        text.contains("\\u{1b}"),
        "the name should show escaped: {text}"
    );
    assert!(
        !text.contains("not-a-real-token"),
        "env value shown: {text}"
    );
}

#[test]
fn check_strict_changes_the_exit_code_only_when_it_should() {
    let dir = home();
    let risky = run_binary(dir.path(), &["check", "--strict", &fixture_arg("risky")]);
    assert_eq!(risky.code, exit::FINDINGS);
    let clean = run_binary(dir.path(), &["check", "--strict", &fixture_arg("clean")]);
    assert_eq!(clean.code, exit::OK, "{}", clean.all());
}

#[test]
fn check_escapes_a_hostile_project_path_and_broken_yaml() {
    let dir = home();
    let hostile = dir.path().join("evil\u{1b}[31m\u{202e}name");
    fs::create_dir(&hostile).unwrap();
    fs::write(hostile.join("compose.yaml"), "services: [\u{1b}[2J\n").unwrap();
    let done = run_binary(dir.path(), &["check", hostile.to_str().unwrap()]);
    assert_eq!(done.code, exit::FAILED, "{}", done.all());
    let text = done.all();
    assert!(!text.contains('\u{1b}'), "{text:?}");
    assert!(!text.contains('\u{202e}'), "{text:?}");
    assert!(text.contains("not as clean"));
}

#[test]
fn check_reports_a_missing_folder() {
    let dir = home();
    let done = run_binary(dir.path(), &["check", "/no/such/folder/here"]);
    assert_eq!(done.code, exit::FAILED);
}

#[test]
fn check_refuses_a_mac_only_project_under_strict() {
    let dir = home();
    fs::create_dir(dir.path().join("App.xcodeproj")).unwrap();
    let plain = run_binary(dir.path(), &["check"]);
    assert_eq!(plain.code, exit::OK);
    assert!(plain.out.contains("Refused"));
    let strict = run_binary(dir.path(), &["check", "--strict"]);
    assert_eq!(strict.code, exit::FINDINGS);
}

#[test]
fn check_never_prints_env_file_contents() {
    let dir = home();
    fs::write(dir.path().join(".env"), "SECRET_TOKEN=do-not-print-me\n").unwrap();
    fs::write(
        dir.path().join("compose.yaml"),
        "services:\n  a:\n    image: x\n    env_file: .env\n",
    )
    .unwrap();
    let done = run_binary(dir.path(), &["check"]);
    assert!(!done.all().contains("do-not-print-me"));
}

#[test]
fn resources_rejects_bad_overrides() {
    let dir = home();
    let done = run_binary(dir.path(), &["resources", "--memory", "lots"]);
    assert_eq!(done.code, exit::USAGE);
    let done = run_binary(dir.path(), &["resources", "--cpus", "0"]);
    assert_eq!(done.code, exit::USAGE);
}

#[test]
fn net_check_explains_allow_and_deny() {
    let dir = home();
    let policy = fixture_arg("policy.yaml");
    let allow = run_binary(
        dir.path(),
        &[
            "net",
            "check",
            &policy,
            "GET",
            "https://registry.npmjs.org/left-pad",
        ],
    );
    assert_eq!(allow.code, exit::OK, "{}", allow.all());
    assert!(allow.out.contains("allow"));
    assert!(allow.out.contains("npm-read-only"));

    let deny = run_binary(
        dir.path(),
        &[
            "net",
            "check",
            &policy,
            "PUT",
            "https://registry.npmjs.org/left-pad",
        ],
    );
    assert_eq!(deny.code, exit::DENIED);
    assert!(deny.out.contains("deny"));
    assert!(deny.out.contains("does not allow this method"));

    let meta = run_binary(
        dir.path(),
        &[
            "net",
            "check",
            &policy,
            "GET",
            "http://169.254.169.254/latest",
        ],
    );
    assert_eq!(meta.code, exit::DENIED);

    let unlisted = run_binary(
        dir.path(),
        &["net", "check", &policy, "GET", "https://example.com/"],
    );
    assert_eq!(unlisted.code, exit::DENIED);
    assert!(unlisted.out.contains("no rule matches"));
}

#[test]
fn net_check_hides_credentials_in_the_url() {
    let dir = home();
    let done = run_binary(
        dir.path(),
        &[
            "net",
            "check",
            &fixture_arg("policy.yaml"),
            "GET",
            "https://user:hunter2@registry.npmjs.org/x?token=abc123",
        ],
    );
    assert_eq!(done.code, exit::DENIED);
    assert!(!done.all().contains("hunter2"));
    assert!(!done.all().contains("abc123"));
}

#[test]
fn net_check_refuses_a_broken_policy_and_escapes_it() {
    let dir = home();
    let policy = dir.path().join("policy.yaml");
    fs::write(&policy, "version: 1\nrules:\n  - name: \"bad\\e[2J\"\n    host: a.example.com\n    ports: [443]\n    methods: [GET]\n").unwrap();
    let done = run_binary(
        dir.path(),
        &[
            "net",
            "check",
            policy.to_str().unwrap(),
            "GET",
            "https://a.example.com/",
        ],
    );
    assert_eq!(done.code, exit::FAILED, "{}", done.all());
    assert!(!done.all().contains('\u{1b}'));
    let done = run_binary(
        dir.path(),
        &[
            "net",
            "check",
            "/no/such/policy.yaml",
            "GET",
            "https://a.example.com/",
        ],
    );
    assert_eq!(done.code, exit::FAILED);
}

#[test]
fn secret_values_on_the_command_line_are_refused_and_not_echoed() {
    let dir = home();
    let attempts: [&[&str]; 5] = [
        &["secrets", "set", "API_KEY", "hunter2-argv"],
        &["secrets", "set", "API_KEY", "--value=hunter2-argv"],
        &["secrets", "set", "API_KEY", "--value", "hunter2-argv"],
        &["secrets", "set", "--value=hunter2-argv", "API_KEY"],
        &["secrets", "rm", "API_KEY", "hunter2-argv"],
    ];
    for args in attempts {
        let done = common::run_binary_expecting_refusal(dir.path(), args);
        assert_eq!(done.code, exit::USAGE, "{args:?}: {}", done.all());
        assert!(
            !done.all().contains("hunter2-argv"),
            "{args:?}: {}",
            done.all()
        );
        assert!(
            done.err.contains("not accepted on the command line"),
            "{args:?}"
        );
        assert!(!dir.path().join(brand::STATE_DIR).exists());
    }
}

#[test]
fn secrets_import_dry_run_prints_names_and_no_values() {
    let dir = home();
    let done = run_binary(
        dir.path(),
        &["secrets", "import", "--dry-run", &fixture_arg("fake.env")],
    );
    assert_eq!(done.code, exit::OK, "{}", done.all());
    assert!(done.out.contains("FAKE_API_KEY"));
    assert!(done.out.contains("FAKE_DB_PASSWORD"));
    assert!(done.out.contains("keychain was not touched"));
    for value in ["aaaa1111", "bbbb2222", "cccc3333"] {
        assert!(!done.all().contains(value), "{value}");
    }
    assert!(!dir.path().join(brand::STATE_DIR).exists());
}

#[test]
fn secrets_import_refuses_a_symlink() {
    let dir = home();
    let link = dir.path().join("linked.env");
    symlink(fixture("fake.env"), &link).unwrap();
    let done = run_binary(
        dir.path(),
        &["secrets", "import", "--dry-run", link.to_str().unwrap()],
    );
    assert_eq!(done.code, exit::FAILED);
    assert!(done.err.contains("symlinks"));
}

#[test]
fn secrets_list_is_empty_and_writes_nothing() {
    let dir = home();
    let done = run_binary(dir.path(), &["secrets", "list"]);
    assert_eq!(done.code, exit::OK);
    assert!(done.out.contains("No secrets recorded"));
    assert!(!dir.path().join(brand::STATE_DIR).exists());
}

#[test]
fn output_has_no_color_when_piped() {
    let dir = home();
    let done = run_binary(dir.path(), &["doctor"]);
    assert!(!done.all().contains('\u{1b}'));
}

fn make_state(home: &std::path::Path) -> std::path::PathBuf {
    let state = home.join(brand::STATE_DIR);
    fs::create_dir(&state).unwrap();
    fs::write(state.join(cli::state::MARKER_FILE), "marker\n").unwrap();
    fs::create_dir(state.join("projects")).unwrap();
    fs::write(state.join("projects/data.json"), "{}").unwrap();
    state
}

#[test]
fn uninstall_without_purge_removes_nothing() {
    let dir = home();
    let state = make_state(dir.path());
    let done = run_binary(dir.path(), &["uninstall"]);
    assert_eq!(done.code, exit::USAGE);
    assert!(state.exists());
    let done = run_binary(dir.path(), &["uninstall", "--yes"]);
    assert_eq!(done.code, exit::USAGE);
    assert!(state.exists());
}

#[test]
fn uninstall_needs_the_exact_name_typed() {
    let dir = home();
    let state = make_state(dir.path());
    for answer in [
        "",
        "no\n",
        "y\n",
        "yes\n",
        &format!("{} \n", brand::PRODUCT_NAME),
        &brand::PRODUCT_NAME.to_uppercase(),
    ] {
        let done = run_binary_with(dir.path(), &["uninstall", "--purge"], &[], answer);
        assert_eq!(done.code, exit::FAILED, "{answer:?}");
        assert!(done.err.contains("Nothing was removed"), "{answer:?}");
        assert!(state.join("projects/data.json").exists(), "{answer:?}");
    }
    let answer = format!("{}\n", brand::PRODUCT_NAME);
    let done = run_binary_with(dir.path(), &["uninstall", "--purge"], &[], &answer);
    assert_eq!(done.code, exit::OK, "{}", done.all());
    assert!(!state.exists());
    assert!(dir.path().exists());
}

#[test]
fn uninstall_lists_exactly_what_it_removes() {
    let dir = home();
    let state = make_state(dir.path());
    let done = run_binary_with(dir.path(), &["uninstall", "--purge"], &[], "");
    assert!(done
        .out
        .contains(state.join("projects/data.json").to_str().unwrap()));
    assert!(done
        .out
        .contains(state.join(cli::state::MARKER_FILE).to_str().unwrap()));
    assert!(done.out.contains("3 items"));
}

#[test]
fn uninstall_yes_skips_the_prompt_but_still_lists() {
    let dir = home();
    let state = make_state(dir.path());
    let done = run_binary(dir.path(), &["uninstall", "--purge", "--yes"]);
    assert_eq!(done.code, exit::OK);
    assert!(done.out.contains("data.json"));
    assert!(!state.exists());
}

#[test]
fn uninstall_refuses_a_symlink_and_leaves_its_target() {
    let dir = home();
    let victim = home();
    fs::write(victim.path().join("keep.txt"), "keep").unwrap();
    fs::write(victim.path().join(cli::state::MARKER_FILE), "marker").unwrap();
    let link = dir.path().join(brand::STATE_DIR);
    symlink(victim.path(), &link).unwrap();
    let done = run_binary(dir.path(), &["uninstall", "--purge", "--yes"]);
    assert_eq!(done.code, exit::FAILED, "{}", done.all());
    assert!(done.err.contains("symbolic link"));
    assert!(victim.path().join("keep.txt").exists());
    assert!(fs::symlink_metadata(&link).is_ok());
}

#[test]
fn uninstall_refuses_a_folder_it_did_not_create() {
    let dir = home();
    let state = dir.path().join(brand::STATE_DIR);
    fs::create_dir(&state).unwrap();
    fs::write(state.join("other-tool-data"), "keep").unwrap();
    let done = run_binary(dir.path(), &["uninstall", "--purge", "--yes"]);
    assert_eq!(done.code, exit::FAILED);
    assert!(state.join("other-tool-data").exists());
}

#[test]
fn uninstall_accepts_a_folder_holding_only_engine_state() {
    let dir = home();
    let engine = dir
        .path()
        .join(brand::STATE_DIR)
        .join(cli::state::ENGINE_DIR);
    fs::create_dir_all(engine.join("cache")).unwrap();
    let done = run_binary(dir.path(), &["uninstall", "--purge", "--yes"]);
    assert_eq!(done.code, exit::OK, "{}", done.all());
    assert!(!engine.exists());
}

#[test]
fn uninstall_refuses_while_a_sandbox_socket_answers() {
    let dir = tempfile::Builder::new()
        .prefix("u")
        .tempdir_in("/tmp")
        .unwrap();
    let state = make_state(dir.path());
    let agents = state.join(cli::state::ENGINE_DIR).join("run/agent");
    fs::create_dir_all(&agents).unwrap();
    let socket = agents.join("a.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let done = run_binary(dir.path(), &["uninstall", "--purge", "--yes"]);
    assert_eq!(done.code, exit::FAILED, "{}", done.all());
    assert!(done.err.contains("VM is running"));
    assert!(state.exists());
    // Once nothing listens, the socket is stale and no longer blocks removal.
    drop(listener);
    let done = run_binary(dir.path(), &["uninstall", "--purge", "--yes"]);
    assert_eq!(done.code, exit::OK, "{}", done.all());
    assert!(!state.exists());
}

#[test]
fn uninstall_refuses_a_file_at_the_state_path() {
    let dir = home();
    let state = dir.path().join(brand::STATE_DIR);
    fs::write(&state, "a file").unwrap();
    let done = run_binary(dir.path(), &["uninstall", "--purge", "--yes"]);
    assert_eq!(done.code, exit::FAILED);
    assert!(state.exists());
}

#[test]
fn uninstall_does_not_follow_links_inside_the_state_folder() {
    let dir = home();
    let outside = home();
    fs::write(outside.path().join("keep.txt"), "keep").unwrap();
    let state = make_state(dir.path());
    symlink(outside.path(), state.join("escape")).unwrap();
    let done = run_binary(dir.path(), &["uninstall", "--purge", "--yes"]);
    assert_eq!(done.code, exit::OK, "{}", done.all());
    assert!(done.out.contains("only the link goes"));
    assert!(!state.exists());
    assert!(outside.path().join("keep.txt").exists());
}

#[test]
fn uninstall_only_ever_removes_the_fixed_folder_name() {
    let dir = home();
    let sibling = dir.path().join("sibling");
    fs::create_dir(&sibling).unwrap();
    fs::write(dir.path().join("file.txt"), "keep").unwrap();
    make_state(dir.path());
    // A bad override is refused outright, rather than falling back to HOME.
    for bad in ["relative", "/tmp/../tmp"] {
        let done = run_binary_with(
            dir.path(),
            &["uninstall", "--purge", "--yes"],
            &[(&common::home_var(), bad)],
            "",
        );
        assert_eq!(done.code, exit::FAILED, "{bad}");
        assert!(dir.path().join(brand::STATE_DIR).exists());
    }
    let done = run_binary(dir.path(), &["uninstall", "--purge", "--yes"]);
    assert_eq!(done.code, exit::OK);
    assert!(sibling.exists());
    assert!(dir.path().join("file.txt").exists());
}

#[test]
fn uninstall_with_no_state_folder_is_a_no_op() {
    let dir = home();
    let done = run_binary(dir.path(), &["uninstall", "--purge", "--yes"]);
    assert_eq!(done.code, exit::OK);
    assert!(done.out.contains("Nothing to remove"));
}
