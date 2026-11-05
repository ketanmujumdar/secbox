//! Runs commands in-process with a memory store and a fake host. These are the
//! only tests that exercise storing secrets, and none can reach the keychain.

mod common;

use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};

use cli::{exit, Hypervisor};
use common::{fixture, read_all, Harness, GIB};

const VALUE: &str = "hunter2-super-secret-value";

fn fixture_arg(name: &str) -> String {
    fixture(name).to_string_lossy().into_owned()
}

#[test]
fn set_stores_the_value_and_never_shows_or_writes_it() {
    let t = Harness::new();
    t.answer(&[VALUE, VALUE]);
    let done = t.run(&["secrets", "set", "API_KEY"], "");
    assert_eq!(done.code, exit::OK, "{}", done.all());
    assert_eq!(t.vault.value("API_KEY").as_deref(), Some(VALUE));
    assert!(!done.all().contains(VALUE));
    assert!(done.out.contains("API_KEY"));
    assert!(done.out.contains("Do not rely on it"));
    let on_disk = read_all(&t.state());
    assert!(on_disk.contains("API_KEY"));
    assert!(!on_disk.contains(VALUE));

    let listed = t.run(&["secrets", "list"], "");
    assert_eq!(listed.code, exit::OK);
    assert!(listed.out.contains("API_KEY"));
    assert!(listed.out.contains("no service"));
    assert!(!listed.all().contains(VALUE));
}

#[test]
fn set_keeps_metadata_owner_only() {
    let t = Harness::new();
    t.answer(&[VALUE, VALUE]);
    assert_eq!(t.run(&["secrets", "set", "API_KEY"], "").code, exit::OK);
    let mode = |path: &std::path::Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&t.state()), 0o700);
    let mut pending = vec![t.state()];
    let mut files = 0;
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                assert_eq!(mode(&path), 0o700, "{}", path.display());
                pending.push(path);
            } else {
                assert_eq!(mode(&path), 0o600, "{}", path.display());
                files += 1;
            }
        }
    }
    assert_eq!(files, 3, "marker, metadata and audit log");
}

#[test]
fn set_tightens_a_state_folder_someone_loosened() {
    let t = Harness::new();
    t.answer(&[VALUE, VALUE]);
    assert_eq!(t.run(&["secrets", "set", "A"], "").code, exit::OK);
    fs::set_permissions(t.state(), fs::Permissions::from_mode(0o755)).unwrap();
    t.answer(&[VALUE, VALUE]);
    assert_eq!(t.run(&["secrets", "set", "B"], "").code, exit::OK);
    let mode = fs::metadata(t.state()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
}

#[test]
fn set_refuses_mismatch_empty_and_no_terminal_without_storing() {
    let t = Harness::new();
    t.answer(&[VALUE, "something else"]);
    let done = t.run(&["secrets", "set", "API_KEY"], "");
    assert_eq!(done.code, exit::FAILED);
    assert!(done.err.contains("differ"));
    assert!(!done.all().contains(VALUE));
    assert_eq!(t.vault.value("API_KEY"), None);

    t.answer(&[""]);
    let done = t.run(&["secrets", "set", "API_KEY"], "");
    assert_eq!(done.code, exit::FAILED);
    assert_eq!(t.vault.value("API_KEY"), None);

    t.answer(&[]);
    let done = t.run(&["secrets", "set", "API_KEY"], "");
    assert_eq!(done.code, exit::FAILED);
    assert!(done.err.contains("terminal"));
}

#[test]
fn set_refuses_a_name_that_looks_like_a_pasted_value_without_echoing_it() {
    let t = Harness::new();
    let done = t.run(&["secrets", "set", "API_KEY=hunter2-pasted"], "");
    assert_eq!(done.code, exit::FAILED);
    assert!(!done.all().contains("hunter2-pasted"));
    assert!(!t.state().exists());
}

#[test]
fn set_records_assignments_and_validates_them_before_prompting() {
    let t = Harness::new();
    t.answer(&[VALUE, VALUE]);
    let done = t.run(
        &[
            "secrets",
            "set",
            "API_KEY",
            "--service",
            "web",
            "--service",
            "worker",
            "--proxy-host",
            "api.anthropic.com",
        ],
        "",
    );
    assert_eq!(done.code, exit::OK, "{}", done.all());
    assert!(done
        .out
        .contains("web, worker: proxy for api.anthropic.com"));
    let listed = t.run(&["secrets", "list"], "");
    assert!(listed
        .out
        .contains("web, worker: proxy for api.anthropic.com"));

    t.answer(&[]);
    let done = t.run(
        &["secrets", "set", "B", "--proxy-host", "api.anthropic.com"],
        "",
    );
    assert_eq!(done.code, exit::FAILED);
    assert!(done.err.contains("--service"));
    let done = t.run(
        &[
            "secrets",
            "set",
            "B",
            "--service",
            "web",
            "--proxy-host",
            "*.example.com",
        ],
        "",
    );
    assert_eq!(done.code, exit::FAILED);
    assert!(done.err.contains("bound host"));
    let done = t.run(
        &[
            "secrets",
            "set",
            "B",
            "--service",
            "web",
            "--service",
            "web",
        ],
        "",
    );
    assert_eq!(done.code, exit::FAILED);
    assert_eq!(t.vault.value("B"), None);
}

#[test]
fn import_stores_values_as_data_and_prints_names_only() {
    let t = Harness::new();
    let done = t.run(&["secrets", "import", &fixture_arg("fake.env")], "");
    assert_eq!(done.code, exit::OK, "{}", done.all());
    assert!(done.out.contains("FAKE_API_KEY"));
    assert!(done.out.contains("3 stored"));
    assert!(done.out.contains("Skipped    FAKE_EMPTY"));
    for value in ["aaaa1111", "bbbb2222", "cccc3333"] {
        assert!(!done.all().contains(value), "{value}");
    }
    assert_eq!(
        t.vault.value("FAKE_API_KEY").as_deref(),
        Some("fake-value-do-not-use-aaaa1111")
    );
    assert_eq!(
        t.vault.value("FAKE_EXPANSION").as_deref(),
        Some("$(touch /tmp/should-never-run)cccc3333")
    );
    assert!(!std::path::Path::new("/tmp/should-never-run").exists());
    let on_disk = read_all(&t.state());
    assert!(!on_disk.contains("aaaa1111"));
    assert!(on_disk.contains("imported"), "audit log: {on_disk}");
    let again = t.run(&["secrets", "import", &fixture_arg("fake.env")], "");
    assert!(again.out.contains("Replaced"));
}

#[test]
fn import_dry_run_stores_nothing() {
    let t = Harness::new();
    let done = t.run(
        &["secrets", "import", "--dry-run", &fixture_arg("fake.env")],
        "",
    );
    assert_eq!(done.code, exit::OK);
    assert_eq!(t.vault.value("FAKE_API_KEY"), None);
    assert!(!t.state().exists());
}

#[test]
fn import_reports_line_numbers_and_never_the_bad_line() {
    let t = Harness::new();
    let bad = t.project.path().join("bad.env");
    fs::write(&bad, "GOOD=1\nsk-hunter2-not-an-assignment\n").unwrap();
    let done = t.run(&["secrets", "import", bad.to_str().unwrap()], "");
    assert_eq!(done.code, exit::FAILED);
    assert!(done.err.contains("line 2"), "{}", done.err);
    assert!(!done.all().contains("hunter2"));
    assert_eq!(t.vault.value("GOOD"), None);
}

#[test]
fn rm_removes_the_value_and_the_record() {
    let t = Harness::new();
    t.answer(&[VALUE, VALUE]);
    let added = t.run(&["secrets", "set", "API_KEY", "--service", "web"], "");
    assert_eq!(added.code, exit::OK);
    let done = t.run(&["secrets", "rm", "API_KEY"], "");
    assert_eq!(done.code, exit::OK, "{}", done.all());
    assert_eq!(t.vault.value("API_KEY"), None);
    assert!(t
        .run(&["secrets", "list"], "")
        .out
        .contains("No secrets recorded"));
    let missing = t.run(&["secrets", "rm", "API_KEY"], "");
    assert_eq!(missing.code, exit::FAILED);
}

#[test]
fn secrets_are_kept_per_project() {
    let t = Harness::new();
    let other = tempfile::TempDir::new().unwrap();
    let other_path = other.path().to_str().unwrap();
    t.answer(&[VALUE, VALUE]);
    assert_eq!(t.run(&["secrets", "set", "API_KEY"], "").code, exit::OK);
    let elsewhere = t.run(&["secrets", "list", "--project", other_path], "");
    assert!(elsewhere.out.contains("No secrets recorded"));
    let removed = t.run(&["secrets", "rm", "API_KEY", "--project", other_path], "");
    assert_eq!(removed.code, exit::FAILED);
    assert_eq!(t.vault.value("API_KEY").as_deref(), Some(VALUE));
}

#[test]
fn secrets_refuse_a_symlink_at_the_state_path() {
    let t = Harness::new();
    let victim = tempfile::TempDir::new().unwrap();
    symlink(victim.path(), t.state()).unwrap();
    t.answer(&[VALUE, VALUE]);
    let done = t.run(&["secrets", "set", "API_KEY"], "");
    assert_eq!(done.code, exit::FAILED);
    assert!(done.err.contains("symbolic link"));
    assert_eq!(t.vault.value("API_KEY"), None);
    assert_eq!(fs::read_dir(victim.path()).unwrap().count(), 0);
}

#[test]
fn damaged_metadata_is_an_error_not_an_empty_list() {
    let t = Harness::new();
    t.answer(&[VALUE, VALUE]);
    assert_eq!(t.run(&["secrets", "set", "API_KEY"], "").code, exit::OK);
    let projects = t.state().join("projects");
    let project = fs::read_dir(projects)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(project.join("secrets.json"), "{not json").unwrap();
    let done = t.run(&["secrets", "list"], "");
    assert_eq!(done.code, exit::FAILED);
    assert!(done.err.contains("damaged"));
}

#[test]
fn uninstall_warns_that_keychain_entries_stay() {
    let t = Harness::new();
    t.answer(&[VALUE, VALUE]);
    assert_eq!(t.run(&["secrets", "set", "API_KEY"], "").code, exit::OK);
    let done = t.run(&["uninstall", "--purge"], "no\n");
    assert_eq!(done.code, exit::FAILED);
    assert!(done.out.contains("does not touch the OS keychain"));
    assert!(done.out.contains("/API_KEY"));
    assert!(!done.all().contains(VALUE));
    assert!(t.state().exists());
    let answer = format!("{}\n", brand::PRODUCT_NAME);
    let done = t.run(&["uninstall", "--purge"], &answer);
    assert_eq!(done.code, exit::OK);
    assert!(!t.state().exists());
    assert_eq!(t.vault.value("API_KEY").as_deref(), Some(VALUE));
}

#[test]
fn resources_lowers_a_hostile_request_to_the_maximum() {
    let t = Harness::new();
    let done = t.run(&["resources", &fixture_arg("risky")], "");
    assert_eq!(done.code, exit::OK, "{}", done.all());
    assert!(done.out.contains("999T"));
    assert!(done.out.contains("18G"), "{}", done.out);
    assert!(done.out.contains("Lowered memory"));
    assert!(done.out.contains("Lowered CPUs"));
    assert!(done.out.contains("not applied"));
}

#[test]
fn resources_overrides_replace_the_repo_values() {
    let t = Harness::new();
    let done = t.run(
        &[
            "resources",
            &fixture_arg("clean"),
            "--memory",
            "6G",
            "--cpus",
            "1.5",
        ],
        "",
    );
    assert_eq!(done.code, exit::OK, "{}", done.all());
    let memory = done.out.lines().find(|l| l.starts_with("Memory")).unwrap();
    assert!(
        memory.contains("2G") && memory.trim_end().ends_with("6G"),
        "{memory}"
    );
    let cpus = done.out.lines().find(|l| l.starts_with("CPUs")).unwrap();
    assert!(cpus.trim_end().ends_with("1.500"), "{cpus}");
}

#[test]
fn resources_uses_defaults_with_no_config() {
    let t = Harness::new();
    let done = t.run(&["resources"], "");
    assert_eq!(done.code, exit::OK);
    let memory = done.out.lines().find(|l| l.starts_with("Memory")).unwrap();
    assert!(memory.trim_end().ends_with("4G"), "{memory}");
}

#[test]
fn resources_refuses_when_the_host_would_be_left_short() {
    let mut t = Harness::new();
    t.host.memory_available = Some(2 * GIB + GIB / 2);
    let done = t.run(&["resources"], "");
    assert_eq!(done.code, exit::FAILED);
    assert!(done.err.contains("Refused"));
    assert!(done.err.contains("not enough memory"));
}

#[test]
fn resources_refuses_a_bad_config_and_escapes_it() {
    let t = Harness::new();
    fs::write(
        t.project.path().join(brand::CONFIG_FILE),
        "resources:\n  \"evil\\e[2J\": 1\n",
    )
    .unwrap();
    let done = t.run(&["resources"], "");
    assert_eq!(done.code, exit::FAILED);
    assert!(!done.all().contains('\u{1b}'));
    assert!(done.err.contains("unknown key"));
}

#[test]
fn resources_refuses_a_config_that_links_out_of_the_project() {
    let t = Harness::new();
    let outside = tempfile::TempDir::new().unwrap();
    let target = outside.path().join("config.yaml");
    fs::write(&target, "resources:\n  memory: 2G\n").unwrap();
    symlink(&target, t.project.path().join(brand::CONFIG_FILE)).unwrap();
    assert_eq!(t.run(&["resources"], "").code, exit::FAILED);
}

#[test]
fn doctor_reports_ok_warn_and_fail_with_next_steps() {
    let mut t = Harness::new();
    let done = t.run(&["doctor"], "");
    assert_eq!(done.code, exit::OK, "{}", done.out);
    assert!(done.out.contains("Suggested"));
    assert!(
        done.out.contains("4G memory, 2 CPUs, 20G disk"),
        "{}",
        done.out
    );

    t.host.memory_total = Some(8 * GIB);
    let done = t.run(&["doctor"], "");
    assert_eq!(done.code, exit::OK);
    assert!(done.out.contains("16 GB is recommended"));

    t.host.hypervisor = Hypervisor::Missing {
        detail: "no hypervisor here".to_string(),
        next: "try another machine".to_string(),
    };
    let done = t.run(&["doctor"], "");
    assert_eq!(done.code, exit::FAILED);
    assert!(done.out.contains("no hypervisor here"));
    assert!(done.out.contains("next  try another machine"));
}

#[test]
fn doctor_fails_on_a_full_disk() {
    let mut t = Harness::new();
    t.host.disk_free = Some(GIB);
    assert_eq!(t.run(&["doctor"], "").code, exit::FAILED);
}

#[test]
fn doctor_escapes_what_a_tool_reports() {
    let mut t = Harness::new();
    t.host.os = "macOS \u{1b}[2J".to_string();
    let done = t.run(&["doctor"], "");
    assert!(!done.all().contains('\u{1b}'));
}

#[test]
fn color_is_used_only_when_asked_and_stays_out_of_escaped_text() {
    let mut t = Harness::new();
    let plain = t.run(&["doctor"], "");
    assert!(!plain.out.contains('\u{1b}'));
    t.color = true;
    let colored = t.run(&["doctor"], "");
    assert!(colored.out.contains("\u{1b}[32mok\u{1b}[0m"));
}
