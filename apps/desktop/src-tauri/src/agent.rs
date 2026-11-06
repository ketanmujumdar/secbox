//! The Connect button: Claude Code, working in one project's sandbox.
//!
//! This does nothing the command line cannot. It runs `mcp setup claude` with the
//! bundled CLI, which makes a folder that denies Claude Code its own shell, file
//! and web tools, then opens a terminal in that folder.
//!
//! The terminal is opened through a small script of ours, so nothing from the
//! project reaches a command line: the folder's name is the project's identity,
//! which is already reduced to safe characters, and the script finds its folder
//! from where it is.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

const LAUNCHER: &str = "start-claude.command";
const SERVERS_FILE: &str = ".mcp.json";
const LAUNCHER_TEXT: &str = r#"#!/bin/sh
# Starts Claude Code here, where its own shell, file and web tools are denied.
cd "$(dirname "$0")" || exit 1
if ! command -v claude >/dev/null 2>&1; then
  echo "Claude Code is not installed, or not on this terminal's PATH."
  exit 1
fi
exec claude
"#;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Connected {
    pub folder: String,
    /// What to run by hand, if a terminal could not be opened.
    pub command: String,
    pub opened: bool,
}

/// One folder per project, outside every project, kept between uses.
pub fn folder_for(home: &Path, project: &Path) -> Result<PathBuf, String> {
    let real = std::fs::canonicalize(project).map_err(|e| format!("cannot open the project: {}", e.kind()))?;
    let id = hoststate::project_id(&real)?;
    Ok(home.join(format!("{}-agents", brand::CLI_BIN)).join(id.as_str()))
}

/// Makes the folder if it is not there, and returns the launcher in it.
pub fn prepare(cli: &Path, folder: &Path, project: &Path) -> Result<PathBuf, String> {
    let servers = folder.join(SERVERS_FILE);
    if servers.is_file() {
        repoint(&servers, cli)?;
    } else {
        make_folder(cli, folder, project)?;
    }
    let launcher = folder.join(LAUNCHER);
    write_launcher(&launcher)?;
    Ok(launcher)
}

fn make_folder(cli: &Path, folder: &Path, project: &Path) -> Result<(), String> {
    if let Some(parent) = folder.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot make the agents folder: {}", e.kind()))?;
    }
    let done = Command::new(cli)
        .args(["mcp", "setup", "claude"])
        .arg(folder)
        .arg("--project")
        .arg(project)
        .output()
        .map_err(|e| format!("cannot run {}: {}", brand::CLI_BIN, e.kind()))?;
    if done.status.success() {
        return Ok(());
    }
    Err(proto::text::escape_short(String::from_utf8_lossy(&done.stderr).trim(), 300))
}

/// The folder names the program it starts, which moves when the app does.
fn repoint(servers: &Path, cli: &Path) -> Result<(), String> {
    let bad = || "the agent folder's server list could not be read".to_owned();
    let text = std::fs::read(servers).map_err(|_| bad())?;
    let mut list: serde_json::Value = serde_json::from_slice(&text).map_err(|_| bad())?;
    let cli = cli.to_string_lossy();
    let mut changed = false;
    for server in list["mcpServers"].as_object_mut().ok_or_else(bad)?.values_mut() {
        if server["command"].as_str() != Some(cli.as_ref()) {
            server["command"] = serde_json::Value::from(cli.as_ref());
            changed = true;
        }
    }
    if !changed {
        return Ok(());
    }
    let text = serde_json::to_vec_pretty(&list).map_err(|_| bad())?;
    hoststate::write_private(servers, &text)
}

fn write_launcher(launcher: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    hoststate::write_private(launcher, LAUNCHER_TEXT.as_bytes())?;
    std::fs::set_permissions(launcher, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("cannot make the launcher runnable: {}", e.kind()))
}

/// Opens a terminal running the launcher. Only a Mac has a way this simple, and
/// elsewhere the window shows the command instead.
fn open_terminal(launcher: &Path) -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    Command::new("/usr/bin/open")
        .args(["-a", "Terminal"])
        .arg(launcher)
        .status()
        .is_ok_and(|status| status.success())
}

pub fn connect(cli: &Path, home: &Path, project: &Path) -> Result<Connected, String> {
    let folder = folder_for(home, project)?;
    let launcher = prepare(cli, &folder, project)?;
    Ok(Connected {
        folder: proto::text::escape_path(&folder),
        command: format!("cd {} && claude", proto::text::escape_path(&folder)),
        opened: open_terminal(&launcher),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Stands in for the CLI: records its arguments, and makes what setup makes.
    fn fake_cli(dir: &Path, exit: u8) -> PathBuf {
        let path = dir.join("fake-cli");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {log}\nif [ {exit} != 0 ]; then echo 'it went wrong' >&2; exit {exit}; fi\nmkdir -p \"$4\"\nprintf '{{\"mcpServers\":{{\"x\":{{\"command\":\"/old/place\"}}}}}}' > \"$4/.mcp.json\"\n",
            log = dir.join("args").display()
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn the_folder_is_outside_the_project_and_the_same_each_time() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let first = folder_for(home.path(), project.path()).unwrap();
        assert_eq!(first, folder_for(home.path(), project.path()).unwrap());
        assert!(first.starts_with(home.path()));
        assert!(!first.starts_with(project.path()));
    }

    #[test]
    fn it_runs_setup_for_this_project_and_leaves_a_runnable_launcher() {
        let tools = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let folder = tools.path().join("agents/one");
        let cli = fake_cli(tools.path(), 0);
        let launcher = prepare(&cli, &folder, project.path()).unwrap();

        let args = std::fs::read_to_string(tools.path().join("args")).unwrap();
        let args: Vec<&str> = args.lines().collect();
        assert_eq!(args[..3], ["mcp", "setup", "claude"]);
        assert_eq!(args[3], folder.to_str().unwrap());
        assert_eq!(args[4], "--project");
        assert_eq!(args[5], project.path().to_str().unwrap());

        let mode = std::fs::metadata(&launcher).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        let text = std::fs::read_to_string(&launcher).unwrap();
        assert!(text.contains("exec claude"));
        assert!(!text.contains(project.path().to_str().unwrap()), "no project path in a script");
    }

    #[test]
    fn a_second_connect_reuses_the_folder_and_follows_the_program_if_it_moved() {
        let tools = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let folder = tools.path().join("one");
        let cli = fake_cli(tools.path(), 0);
        prepare(&cli, &folder, project.path()).unwrap();
        std::fs::remove_file(tools.path().join("args")).unwrap();

        prepare(&cli, &folder, project.path()).unwrap();
        assert!(!tools.path().join("args").exists(), "setup must not run again");
        let list: serde_json::Value =
            serde_json::from_slice(&std::fs::read(folder.join(SERVERS_FILE)).unwrap()).unwrap();
        assert_eq!(list["mcpServers"]["x"]["command"], cli.to_str().unwrap());
    }

    #[test]
    fn a_failed_setup_says_why_and_leaves_no_launcher() {
        let tools = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let folder = tools.path().join("one");
        let cli = fake_cli(tools.path(), 3);
        let error = prepare(&cli, &folder, project.path()).unwrap_err();
        assert!(error.contains("it went wrong"), "{error}");
        assert!(!folder.join(LAUNCHER).exists());
    }

    #[test]
    fn the_launcher_starts_claude_from_its_own_folder() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = dir.path().join(LAUNCHER);
        write_launcher(&launcher).unwrap();
        let bin = tempfile::tempdir().unwrap();
        std::fs::write(bin.path().join("claude"), "#!/bin/sh\npwd\n").unwrap();
        std::fs::set_permissions(bin.path().join("claude"), std::fs::Permissions::from_mode(0o755)).unwrap();

        let run = |path: String| Command::new(&launcher).current_dir("/").env("PATH", path).output().unwrap();
        let found = run(format!("{}:/usr/bin:/bin", bin.path().display()));
        assert!(found.status.success());
        let shown = String::from_utf8(found.stdout).unwrap();
        assert_eq!(
            std::fs::canonicalize(shown.trim()).unwrap(),
            std::fs::canonicalize(dir.path()).unwrap()
        );

        let missing = run("/usr/bin:/bin".to_owned());
        assert!(!missing.status.success());
        assert!(String::from_utf8(missing.stdout).unwrap().contains("not installed"));
    }
}
