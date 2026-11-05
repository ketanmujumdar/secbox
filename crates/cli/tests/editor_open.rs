//! The editor gate, through the real binary.
//!
//! These run only once `open` is wired into the command line. Until then they
//! say so and stop, rather than reporting a pass they did not earn.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The binary sits next to the `deps` folder holding this test executable.
/// The usual compile-time variable would write the product name in this file.
fn binary() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join(brand::CLI_BIN)
}

fn home_var() -> String {
    format!("{}{}", brand::ENV_PREFIX, brand::HOME_ENV_SUFFIX)
}

struct Out {
    code: i32,
    text: String,
}

fn run(home: &Path, cwd: &Path, args: &[&str]) -> Out {
    let done = Command::new(binary())
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env(home_var(), home)
        // Something real to start, so no test ever needs an editor installed.
        .env(format!("{}EDITOR_BIN", brand::ENV_PREFIX), "/usr/bin/true")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    Out {
        code: done.status.code().unwrap_or(-1),
        text: format!(
            "{}{}",
            String::from_utf8_lossy(&done.stdout),
            String::from_utf8_lossy(&done.stderr)
        ),
    }
}

/// True once the command exists. The help text is the only place to ask.
fn wired(home: &Path) -> bool {
    run(home, home, &["--help"]).text.contains("\n  open")
}

const FOLDER_OPEN: &str = r#"{
  "version": "2.0.0",
  "tasks": [
    { "label": "planted", "type": "shell", "command": "id",
      "runOptions": { "runOn": "folderOpen" } }
  ]
}"#;

#[test]
fn open_refuses_a_planted_folder_open_task_and_a_repo_cannot_disable_the_check() {
    let home = tempfile::Builder::new()
        .prefix("h")
        .tempdir_in("/tmp")
        .unwrap();
    if !wired(home.path()) {
        eprintln!("SKIPPED: `open` is not wired into the command line yet");
        return;
    }
    let project = tempfile::TempDir::new().unwrap();
    let root = project.path();
    fs::create_dir_all(root.join(".vscode")).unwrap();
    fs::write(root.join(".vscode/tasks.json"), FOLDER_OPEN).unwrap();
    // Everything a hostile repo could try to turn the check off with.
    fs::write(
        root.join(brand::CONFIG_FILE),
        "checks:\n  project-folder-open-tasks: off\n",
    )
    .unwrap();
    fs::write(
        root.join("editor-checks.json"),
        r#"{"version":1,"off":["project-folder-open-tasks"]}"#,
    )
    .unwrap();

    let refused = run(home.path(), root, &["open", "--dry-run"]);
    assert_eq!(refused.code, 1, "{}", refused.text);
    assert!(
        refused.text.contains("project-folder-open-tasks"),
        "{}",
        refused.text
    );
    assert!(refused.text.contains("label planted"), "{}", refused.text);
    assert!(!refused.text.contains("Would start"), "{}", refused.text);

    // The user, and only the user, can turn it off.
    let off = run(
        home.path(),
        root,
        &["checks", "off", "project-folder-open-tasks"],
    );
    assert_eq!(off.code, 0, "{}", off.text);
    let allowed = run(home.path(), root, &["open", "--dry-run"]);
    assert_eq!(allowed.code, 0, "{}", allowed.text);
}

#[test]
fn forcing_the_gate_is_recorded() {
    let home = tempfile::Builder::new()
        .prefix("h")
        .tempdir_in("/tmp")
        .unwrap();
    if !wired(home.path()) {
        eprintln!("SKIPPED: `open` is not wired into the command line yet");
        return;
    }
    let project = tempfile::TempDir::new().unwrap();
    let root = project.path();
    fs::create_dir_all(root.join(".vscode")).unwrap();
    fs::write(root.join(".vscode/tasks.json"), FOLDER_OPEN).unwrap();
    let forced = run(home.path(), root, &["open", "--dry-run", "--force"]);
    assert_eq!(forced.code, 0, "{}", forced.text);
    let log = fs::read_to_string(
        home.path()
            .join(brand::STATE_DIR)
            .join("editor-audit.jsonl"),
    )
    .expect("the override was not recorded");
    assert!(log.contains("editor_open_override"), "{log}");
}
