//! VS Code on a running sandbox's files, with nothing copied to this machine.
//!
//! `open --sandbox` starts VS Code in a profile of its own, holding only the
//! extension in `apps/vscode`, on a virtual folder. Every file operation comes
//! back here, to `editor-bridge`, which runs in the sandbox what the extension
//! asks for, one request per line of JSON. Each one goes through the daemon's
//! `terminal` check, so a stopped sandbox is refused and the request is recorded.
//!
//! Tested on VS Code 1.142: a `.vscode/settings.json` served through a virtual
//! folder is obeyed, and its terminal profile runs a command on this machine.
//! So what a project can use to steer the editor is refused here, on the host,
//! whatever the extension does: see [`is_hidden`]. Nothing from a project is
//! ever written to this machine.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use clap::Args;
use proto::text::{escape, escape_short};
use proto::{ProjectArgs, Reply, Request, SandboxState};
use serde_json::{json, Value};

use super::launch;
use super::paths::Editor;
use crate::debug::prepare_terminal;
use crate::mcp::run::{run_captured, Captured};
use crate::state::StatePaths;
use crate::ui::esc;
use crate::{exit, project_dir, say, tell, Deps};

const WORK: &str = "/work";
/// VS Code keeps a socket in its profile, and macOS limits a socket's path.
const MAX_PROFILE_PATH: usize = 80;
const FILE_LIMIT: usize = 5 * 1024 * 1024;
const LIST_LIMIT: usize = 2 * 1024 * 1024;
const SEARCH_LIMIT: usize = 512 * 1024;
const SEARCH_MATCHES: usize = 1000;
const SEARCH_LINE_CHARS: usize = 300;
const MAX_QUERY_BYTES: usize = 1000;
const FILE_SECONDS: u64 = 60;
const SEARCH_SECONDS: u64 = 60;
const DAEMON_LIMIT: Duration = Duration::from_secs(30);

// Exit codes the scripts below use to say why.
const NOT_FOUND: i32 = 44;
const IS_DIR: i32 = 45;
const EXISTS: i32 = 46;

const EXTENSION_MANIFEST: &str = include_str!("../../../../apps/vscode/package.json");
const EXTENSION_CODE: &str = include_str!("../../../../apps/vscode/extension.js");

/// What the sandbox can use to steer the editor or Git on this machine.
///
/// - `.vscode`: settings, tasks and MCP servers, which VS Code obeys.
/// - `.git`: repository config, and Git here goes through the daemon.
/// - `*.code-workspace`: VS Code offers to open one, with its own settings.
///
/// Names are compared the way a case-insensitive filesystem would, with
/// invisible characters and trailing dots and spaces taken off.
fn is_hidden(part: &str) -> bool {
    let folded: String = part
        .chars()
        .filter(|c| !c.is_control() && !is_invisible(*c))
        .flat_map(char::to_lowercase)
        .collect();
    let name = folded.trim_end_matches(['.', ' ']);
    name == ".vscode" || name == ".git" || name.ends_with(".code-workspace")
}

fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{180E}'
        | '\u{200B}'..='\u{200F}'
        | '\u{2028}'..='\u{202E}'
        | '\u{2060}'..='\u{206F}'
        | '\u{FE00}'..='\u{FE0F}'
        | '\u{FEFF}')
}

/// A name that would read as something else on screen. It is left out of what
/// the editor sees, and the Changes tab flags it.
fn is_shown_name(name: &str) -> bool {
    !name.is_empty() && !name.chars().any(|c| c.is_control() || is_invisible(c))
}

struct Refused {
    code: &'static str,
    message: String,
}

impl Refused {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// `path` from the extension as a path in the sandbox. Empty means `/work`.
fn editor_path(given: &str) -> Result<String, Refused> {
    let given = given.trim_start_matches('/');
    if given.is_empty() {
        return Ok(WORK.to_owned());
    }
    let path = crate::mcp::tools::guest_path(given).map_err(|m| Refused::new("bad_path", m))?;
    if given.split('/').any(is_hidden) {
        return Err(Refused::new(
            "hidden",
            format!("{} keeps this out of the editor, because it can make the editor run commands on your machine", brand::PRODUCT_NAME),
        ));
    }
    Ok(path)
}

fn file_path(given: &str) -> Result<String, Refused> {
    let path = editor_path(given)?;
    if path == WORK {
        return Err(Refused::new("bad_path", "that is the whole project folder"));
    }
    Ok(path)
}

fn sh(script: &str, args: &[&str]) -> Vec<String> {
    let mut argv = vec!["sh".to_owned(), "-c".to_owned(), script.to_owned(), "sh".to_owned()];
    argv.extend(args.iter().map(|a| (*a).to_owned()));
    argv
}

type Guest<'a> = dyn FnMut(&[String], Vec<u8>, usize, u64) -> Result<Captured, String> + 'a;

fn run(guest: &mut Guest<'_>, argv: &[String], input: Vec<u8>, limit: usize, seconds: u64) -> Result<Captured, Refused> {
    let done = guest(argv, input, limit, seconds).map_err(|m| Refused::new("unavailable", m))?;
    if done.timed_out {
        return Err(Refused::new("failed", "the sandbox took too long, so it was stopped"));
    }
    match done.code {
        Some(0) => Ok(done),
        Some(NOT_FOUND) => Err(Refused::new("not_found", "there is no such file in the sandbox")),
        Some(IS_DIR) => Err(Refused::new("is_dir", "that is a folder")),
        Some(EXISTS) => Err(Refused::new("exists", "that already exists")),
        _ => {
            let said = String::from_utf8_lossy(&done.stderr);
            Err(Refused::new("failed", escape_short(said.trim(), 500)))
        }
    }
}

/// One `stat -c '%f %s %Y'` result, then the rest of the line.
fn parse_stat(line: &str) -> Option<(&'static str, u64, u64, &str)> {
    let mut parts = line.splitn(4, ' ');
    let mode = u32::from_str_radix(parts.next()?, 16).ok()?;
    let size = parts.next()?.parse().ok()?;
    let mtime = parts.next()?.parse().ok()?;
    let kind = match mode & 0o170_000 {
        0o040_000 => "dir",
        0o100_000 => "file",
        _ => "other",
    };
    Some((kind, size, mtime, parts.next().unwrap_or("")))
}

fn stat(path: &str, guest: &mut Guest<'_>) -> Result<Value, Refused> {
    let argv = sh(r#"[ -e "$1" ] || exit 44; stat -L -c '%f %s %Y' "$1""#, &[path]);
    let done = run(guest, &argv, Vec::new(), 4096, FILE_SECONDS)?;
    let text = String::from_utf8_lossy(&done.stdout);
    let (kind, size, mtime, _) = parse_stat(text.trim())
        .ok_or_else(|| Refused::new("failed", "the sandbox's answer about that file was not understood"))?;
    Ok(json!({"kind": kind, "size": size, "mtime": mtime}))
}

fn list(path: &str, guest: &mut Guest<'_>) -> Result<Value, Refused> {
    let script = r#"[ -d "$1" ] || exit 44; find "$1" -mindepth 1 -maxdepth 1 -exec stat -L -c '%f %s %Y %n' {} + 2>/dev/null; exit 0"#;
    let done = run(guest, &sh(script, &[path]), Vec::new(), LIST_LIMIT, FILE_SECONDS)?;
    let prefix = format!("{path}/");
    let mut entries = Vec::new();
    let mut left_out = 0u64;
    for line in done.stdout.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        // A name that is not text cannot be named to the editor, so it is left out.
        let Ok(line) = std::str::from_utf8(line) else {
            left_out += 1;
            continue;
        };
        let Some((kind, size, mtime, full)) = parse_stat(line) else {
            left_out += 1;
            continue;
        };
        let Some(name) = full.strip_prefix(&prefix).filter(|n| !n.contains('/')) else {
            left_out += 1;
            continue;
        };
        if is_hidden(name) {
            continue;
        }
        if !is_shown_name(name) {
            left_out += 1;
            continue;
        }
        entries.push(json!({"name": name, "kind": kind, "size": size, "mtime": mtime}));
    }
    Ok(json!({"entries": entries, "left_out": left_out, "cut": done.stdout_cut}))
}

/// Whole files only. A file cut short and then saved would lose its end.
fn read(path: &str, guest: &mut Guest<'_>) -> Result<Value, Refused> {
    let script = r#"[ -d "$1" ] && exit 45; [ -f "$1" ] || exit 44; cat "$1""#;
    let done = run(guest, &sh(script, &[path]), Vec::new(), FILE_LIMIT, FILE_SECONDS)?;
    if done.stdout_cut {
        return Err(Refused::new(
            "too_large",
            format!("that file is over {} MB, so it is not opened here", FILE_LIMIT / (1024 * 1024)),
        ));
    }
    Ok(json!({"data": to_hex(&done.stdout)}))
}

fn head(relative: &str, guest: &mut Guest<'_>) -> Result<Value, Refused> {
    let script = r#"cd /work && git -c safe.directory=/work -c core.fsmonitor=false --no-pager show "HEAD:$1" 2>/dev/null || exit 44"#;
    let done = run(guest, &sh(script, &[relative]), Vec::new(), FILE_LIMIT, FILE_SECONDS)?;
    if done.stdout_cut {
        return Err(Refused::new("too_large", "that file is too large to compare here"));
    }
    Ok(json!({"data": to_hex(&done.stdout)}))
}

fn write(path: &str, request: &Value, guest: &mut Guest<'_>) -> Result<Value, Refused> {
    let data = request["data"].as_str().and_then(from_hex).ok_or_else(|| Refused::new("bad_request", "no file content was given"))?;
    if data.len() > FILE_LIMIT {
        return Err(Refused::new("too_large", "that file is too large to save from here"));
    }
    let create = if request["create"] == json!(true) { "1" } else { "" };
    let overwrite = if request["overwrite"] == json!(true) { "1" } else { "" };
    let script = r#"[ -d "$1" ] && exit 45
if [ -e "$1" ]; then [ -n "$3" ] || exit 46; else [ -n "$2" ] || exit 44; fi
[ -d "$(dirname "$1")" ] || exit 44
cat > "$1""#;
    run(guest, &sh(script, &[path, create, overwrite]), data, 4096, FILE_SECONDS)?;
    Ok(json!({}))
}

fn remove(path: &str, recursive: bool, guest: &mut Guest<'_>) -> Result<Value, Refused> {
    let script = r#"[ -e "$1" ] || [ -L "$1" ] || exit 44
if [ -d "$1" ] && [ ! -L "$1" ]; then
  if [ -n "$2" ]; then rm -rf "$1"; else rmdir "$1"; fi
else
  rm -f "$1"
fi"#;
    run(guest, &sh(script, &[path, if recursive { "1" } else { "" }]), Vec::new(), 4096, FILE_SECONDS)?;
    Ok(json!({}))
}

fn rename(from: &str, to: &str, overwrite: bool, guest: &mut Guest<'_>) -> Result<Value, Refused> {
    let script = r#"[ -e "$1" ] || [ -L "$1" ] || exit 44
if [ -e "$2" ] && [ -z "$3" ]; then exit 46; fi
mv "$1" "$2""#;
    run(guest, &sh(script, &[from, to, if overwrite { "1" } else { "" }]), Vec::new(), 4096, FILE_SECONDS)?;
    Ok(json!({}))
}

/// Every file's path, for going to one by name. What is hidden stays hidden.
fn files(guest: &mut Guest<'_>) -> Result<Value, Refused> {
    let script = r#"cd /work && find . \( -name .git -o -iname .vscode -o -name node_modules \) -prune -o -type f -print 2>/dev/null | head -n 20000"#;
    let done = run(guest, &sh(script, &[]), Vec::new(), LIST_LIMIT, SEARCH_SECONDS)?;
    let found: Vec<&str> = done
        .stdout
        .split(|b| *b == b'\n')
        .filter_map(|l| std::str::from_utf8(l).ok())
        .filter_map(|l| l.strip_prefix("./"))
        .filter(|p| p.split('/').all(|part| !is_hidden(part) && is_shown_name(part)))
        .collect();
    Ok(json!({"files": found, "cut": done.stdout_cut}))
}

/// `./path:line:text`. The first `:number:` ends the path. A path with that in
/// its own name opens the wrong line at worst.
fn parse_match(line: &str) -> Option<(&str, u64, &str)> {
    let rest = line.strip_prefix("./")?;
    let mut from = 0;
    while let Some(at) = rest[from..].find(':') {
        let start = from + at + 1;
        let tail = &rest[start..];
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && tail.as_bytes().get(digits) == Some(&b':') {
            return Some((&rest[..start - 1], tail[..digits].parse().ok()?, &tail[digits + 1..]));
        }
        from = start;
    }
    None
}

/// The text is an argument, and `-F` reads it as plain text, so nothing in it
/// can change what runs.
fn search(request: &Value, guest: &mut Guest<'_>) -> Result<Value, Refused> {
    let query = request["query"].as_str().unwrap_or("");
    if query.is_empty() || query.len() > MAX_QUERY_BYTES || query.contains(['\n', '\0']) {
        return Err(Refused::new("bad_request", "give one line of text to search for"));
    }
    // `find` does the leaving out, because the sandbox's grep may be BusyBox's,
    // which has no --exclude-dir.
    let script = r#"cd /work && find . \( -name .git -o -iname .vscode -o -name node_modules \) -prune -o -type f -size -2048k -exec grep -HnFs -e "$1" {} + 2>/dev/null | head -n 2000; exit 0"#;
    let done = run(guest, &sh(script, &[query]), Vec::new(), SEARCH_LIMIT, SEARCH_SECONDS)?;
    let mut matches = Vec::new();
    for line in done.stdout.split(|b| *b == b'\n') {
        let Ok(line) = std::str::from_utf8(line) else {
            continue;
        };
        let Some((path, number, text)) = parse_match(line) else {
            continue;
        };
        if path.split('/').any(|part| is_hidden(part) || !is_shown_name(part)) {
            continue;
        }
        matches.push(json!({"path": path, "line": number, "text": escape_short(text, SEARCH_LINE_CHARS)}));
        if matches.len() >= SEARCH_MATCHES {
            break;
        }
    }
    let cut = done.stdout_cut || matches.len() >= SEARCH_MATCHES;
    Ok(json!({"matches": matches, "cut": cut}))
}

fn answer(request: &Value, guest: &mut Guest<'_>) -> Result<Value, Refused> {
    let path = request["path"].as_str().unwrap_or("");
    match request["op"].as_str().unwrap_or("") {
        "stat" => stat(&editor_path(path)?, guest),
        "list" => list(&editor_path(path)?, guest),
        "read" => read(&file_path(path)?, guest),
        "head" => {
            file_path(path)?;
            head(path.trim_start_matches('/'), guest)
        }
        "write" => write(&file_path(path)?, request, guest),
        "mkdir" => {
            let argv = sh(r#"[ -e "$1" ] && exit 46; mkdir "$1""#, &[&file_path(path)?]);
            run(guest, &argv, Vec::new(), 4096, FILE_SECONDS)?;
            Ok(json!({}))
        }
        "delete" => remove(&file_path(path)?, request["recursive"] == json!(true), guest),
        "rename" => {
            let to = request["to"].as_str().unwrap_or("");
            rename(&file_path(path)?, &file_path(to)?, request["overwrite"] == json!(true), guest)
        }
        "files" => files(guest),
        "search" => search(request, guest),
        _ => Err(Refused::new("bad_request", format!("{} does not know that request", brand::PRODUCT_NAME))),
    }
}

fn reply_to(line: &str, guest: &mut Guest<'_>) -> Value {
    let Ok(request) = serde_json::from_str::<Value>(line) else {
        return json!({"id": null, "ok": false, "code": "bad_request", "message": "that was not JSON"});
    };
    let id = request["id"].clone();
    match answer(&request, guest) {
        Ok(mut value) => {
            value["id"] = id;
            value["ok"] = json!(true);
            value
        }
        Err(refused) => json!({"id": id, "ok": false, "code": refused.code, "message": refused.message}),
    }
}

fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok())
        .collect()
}

#[derive(Args)]
pub struct BridgeArgs {
    /// Project folder
    #[arg(long, value_name = "PATH")]
    project: PathBuf,
}

/// `editor-bridge`: answers one line of JSON with one line of JSON until stdin
/// closes. The extension is its only intended caller.
pub fn bridge(args: &BridgeArgs, deps: &mut Deps) -> i32 {
    let mut line = String::new();
    loop {
        line.clear();
        match deps.input.read_line(&mut line) {
            Ok(0) => return exit::OK,
            Ok(_) => {}
            Err(_) => return exit::FAILED,
        }
        let reply = {
            let project = args.project.clone();
            let mut guest = |argv: &[String], input: Vec<u8>, limit: usize, seconds: u64| -> Result<Captured, String> {
                let engine = prepare_terminal(None, Some(&project), argv, false, Some(DAEMON_LIMIT), deps)
                    .map_err(|refusal| refusal.reason)?;
                run_captured(engine, input, limit, Duration::from_secs(seconds))
                    .map_err(|error| format!("cannot run the engine: {}", error.kind()))
            };
            reply_to(line.trim_end(), &mut guest)
        };
        if writeln!(deps.out, "{reply}").and_then(|()| deps.out.flush()).is_err() {
            return exit::OK;
        }
    }
}

/// The extension's id. The extension's own manifest has to agree.
fn extension_id() -> String {
    format!("{}.sandbox", brand::CLI_BIN)
}

/// The name of the only terminal profile in the sandbox's VS Code profile.
fn shell_profile() -> String {
    format!("{} shell", brand::CLI_BIN)
}

/// Set by the extension for each window's terminals, to name its project.
fn project_var() -> String {
    format!("{}EDITOR_PROJECT", brand::ENV_PREFIX)
}

/// `editor-shell`: the only terminal the sandbox's VS Code profile offers.
pub fn editor_shell(deps: &mut Deps) -> i32 {
    let Some(project) = (deps.env)(&project_var()).filter(|p| !p.is_empty()) else {
        tell!(deps, "This terminal does not know its project yet. Close it and open a new one.");
        return exit::FAILED;
    };
    crate::debug::shell(&crate::debug::ShellArgsCli::for_project(PathBuf::from(project)), deps)
}

/// The profile's whole settings, rewritten on each open. The aim is an editor
/// that reaches for nothing on this machine and nothing on the network.
///
/// Tested on VS Code 1.142: naming a profile an extension contributes as the
/// default is ignored, and a terminal opens on this machine. So the only
/// profile left here is one that runs the sandbox's shell. A command typed into
/// a terminal in this window must not land on this machine by surprise.
fn profile_settings(binary: &Path) -> Value {
    let mut shell = json!({});
    shell[shell_profile()] = json!({"path": binary, "args": ["editor-shell"]});
    let local_off = |shell: &Value, names: &[&str]| {
        let mut all = shell.clone();
        for name in names {
            all[*name] = Value::Null;
        }
        all
    };
    let mac = local_off(&shell, &["bash", "zsh", "fish", "tmux", "pwsh"]);
    let linux = local_off(&shell, &["bash", "zsh", "fish", "tmux", "pwsh"]);
    json!({
        "terminal.integrated.profiles.osx": mac,
        "terminal.integrated.profiles.linux": linux,
        "terminal.integrated.automationProfile.osx": {"path": binary, "args": ["editor-shell"]},
        "terminal.integrated.automationProfile.linux": {"path": binary, "args": ["editor-shell"]},
        "terminal.integrated.enablePersistentSessions": false,
        "telemetry.telemetryLevel": "off",
        "update.mode": "none",
        "extensions.autoCheckUpdates": false,
        "extensions.autoUpdate": false,
        "extensions.ignoreRecommendations": true,
        "workbench.enableExperiments": false,
        "workbench.startupEditor": "none",
        "window.restoreWindows": "none",
        // Both keep copies of the sandbox's files in this machine's profile:
        // local history on every save, hot exit for unsaved edits.
        "workbench.localHistory.enabled": false,
        "files.hotExit": "off",
        "task.allowAutomaticTasks": "off",
        "security.workspace.trust.enabled": true,
        "git.enabled": false,
        "git.autoRepositoryDetection": false,
        "github.gitAuthentication": false,
        "chat.mcp.enabled": false,
        "chat.mcp.autostart": "never",
        "chat.disableAIFeatures": true,
        "typescript.disableAutomaticTypeAcquisition": true,
        "typescript.enablePromptUseWorkspaceTsdk": false,
        "typescript.tsserver.pluginPaths": [],
        "npm.autoDetect": "off",
        "terminal.integrated.allowWorkspaceConfiguration": false,
        "terminal.integrated.defaultProfile.osx": shell_profile(),
        "terminal.integrated.defaultProfile.linux": shell_profile(),
        "markdown.preview.scrollEditorWithPreview": false
    })
}

fn extension_version() -> Result<String, String> {
    let manifest: Value = serde_json::from_str(EXTENSION_MANIFEST).map_err(|e| e.to_string())?;
    manifest["version"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "the extension has no version".to_owned())
}

/// Writes the profile and the extension. Only our own folder is touched, and it
/// holds nothing from any project.
fn prepare_profile(root: &Path, binary: &Path) -> Result<(PathBuf, PathBuf), String> {
    let data = root.join("data");
    let extensions = root.join("extensions");
    let write = |path: &Path, body: &str| {
        std::fs::write(path, body).map_err(|e| format!("cannot write {}: {}", esc(&path.display().to_string()), e.kind()))
    };
    let make = |path: &Path| {
        std::fs::create_dir_all(path).map_err(|e| format!("cannot make {}: {}", esc(&path.display().to_string()), e.kind()))
    };
    // Copies of project files an earlier editor kept before these settings
    // existed, or that a user turned back on.
    for kept in [data.join("User/History"), data.join("Backups")] {
        if let Err(e) = std::fs::remove_dir_all(&kept) {
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(format!("cannot remove {}: {}", esc(&kept.display().to_string()), e.kind()));
            }
        }
    }
    make(&data.join("User"))?;
    make(&extensions)?;
    let settings = serde_json::to_string_pretty(&profile_settings(binary)).map_err(|e| e.to_string())?;
    write(&data.join("User/settings.json"), &settings)?;

    let wanted = format!("{}-{}", extension_id(), extension_version()?);
    for entry in std::fs::read_dir(&extensions).map_err(|e| e.to_string())?.flatten() {
        if entry.file_name().to_string_lossy() != wanted && entry.path().is_dir() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    let folder = extensions.join(&wanted);
    make(&folder)?;
    write(&folder.join("package.json"), EXTENSION_MANIFEST)?;
    write(&folder.join("extension.js"), EXTENSION_CODE)?;
    let bridge = serde_json::to_string(&json!({"binary": binary})).map_err(|e| e.to_string())?;
    write(&folder.join("bridge.json"), &bridge)?;
    // VS Code's own record of what is installed. Without it, it looks again.
    let _ = std::fs::remove_file(extensions.join("extensions.json"));
    Ok((data, extensions))
}

fn program_for(editor: Editor, deps: &Deps) -> Result<PathBuf, String> {
    let on_path = launch::resolve(editor, deps.env);
    if on_path.is_ok() || editor != Editor::Code {
        return on_path;
    }
    // An app started from the Finder has a short PATH, so look where it installs.
    [
        "/usr/local/bin/code",
        "/opt/homebrew/bin/code",
        "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| launch::runnable(p))
    .ok_or_else(|| on_path.unwrap_err())
}

fn folder_uri(project: &Path) -> String {
    format!("{}-sandbox://{}/", brand::CLI_BIN, to_hex(project.to_string_lossy().as_bytes()))
}

/// `open --sandbox`.
pub fn open(path: Option<&Path>, editor: Editor, dry_run: bool, deps: &mut Deps) -> i32 {
    let failed = |deps: &mut Deps, reason: &str| {
        tell!(deps, "open: {reason}");
        exit::FAILED
    };
    let root = match project_dir(path, deps) {
        Ok(root) => root,
        Err(reason) => return failed(deps, &reason),
    };
    let request = Request::Status(ProjectArgs {
        project: root.to_string_lossy().into_owned(),
    });
    match deps.daemon.stream(request, &mut |_, _| true, Some(DAEMON_LIMIT)) {
        Ok(Reply::Status(report)) if report.sandbox == SandboxState::Running => {}
        Ok(Reply::Status(_)) => {
            return failed(deps, &format!("the sandbox is not running. Start it with {} up, then try again.", brand::CLI_BIN))
        }
        Ok(_) => return failed(deps, "the daemon answered something else."),
        Err(error) => return failed(deps, &error.for_terminal()),
    }
    let program = match program_for(editor, deps) {
        Ok(program) => program,
        Err(reason) => return failed(deps, &esc(&reason)),
    };
    let state = match StatePaths::locate(deps.env) {
        Ok(state) => state,
        Err(reason) => return failed(deps, &esc(&reason)),
    };
    let profile = state.state.join("vscode");
    if profile.join("data").as_os_str().len() > MAX_PROFILE_PATH {
        return failed(deps, "the state folder's path is too long for the editor's socket on this system.");
    }
    if dry_run {
        say!(deps, "Would open the sandbox of {} in {}, in its own profile.", escape(&root.to_string_lossy()), editor.title());
        return exit::OK;
    }
    let binary = match std::env::current_exe() {
        Ok(binary) => binary,
        Err(error) => return failed(deps, &format!("cannot find this program: {}", error.kind())),
    };
    let (data, extensions) = match prepare_profile(&profile, &binary) {
        Ok(made) => made,
        Err(reason) => return failed(deps, &reason),
    };
    let started = Command::new(&program)
        .arg("--new-window")
        .arg("--user-data-dir")
        .arg(&data)
        .arg("--extensions-dir")
        .arg(&extensions)
        .arg("--folder-uri")
        .arg(folder_uri(&root))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Err(error) = started {
        return failed(deps, &format!("could not start {}: {}", editor.title(), error.kind()));
    }
    say!(deps, "Opening the sandbox in {}, in a profile of its own. Nothing is copied to this machine.", editor.title());
    exit::OK
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(stdout: &[u8]) -> Captured {
        Captured {
            code: Some(0),
            stdout: stdout.to_vec(),
            ..Captured::default()
        }
    }

    fn ask(line: Value, mut guest: impl FnMut(&[String], Vec<u8>) -> Captured) -> Value {
        let mut run = |argv: &[String], input: Vec<u8>, _: usize, _: u64| Ok(guest(argv, input));
        reply_to(&line.to_string(), &mut run)
    }

    /// The test that matters: nothing that can steer the editor reaches the
    /// sandbox as a request, in any spelling, for any operation.
    #[test]
    fn what_can_steer_the_editor_is_refused_before_anything_runs() {
        for path in [
            ".vscode",
            ".vscode/settings.json",
            "/.vscode/tasks.json",
            "sub/.vscode/mcp.json",
            ".VSCode/settings.json",
            ".vscode./settings.json",
            ".vscode /settings.json",
            ".vs\u{200b}code/settings.json",
            ".git",
            ".git/config",
            "a/.GIT/hooks/x",
            "shop.code-workspace",
            "sub/SHOP.Code-Workspace",
        ] {
            for op in ["stat", "list", "read", "head", "write", "mkdir", "delete", "rename"] {
                let reply = ask(
                    json!({"id": 1, "op": op, "path": path, "to": "x", "data": ""}),
                    |argv, _| panic!("{op} {path:?} reached the sandbox: {argv:?}"),
                );
                assert_eq!(reply["ok"], false, "{op} {path:?}");
                assert_eq!(reply["code"], "hidden", "{op} {path:?}: {reply}");
            }
        }
        let reply = ask(
            json!({"id": 1, "op": "rename", "path": "a", "to": ".vscode/settings.json"}),
            |argv, _| panic!("reached the sandbox: {argv:?}"),
        );
        assert_eq!(reply["code"], "hidden");
    }

    #[test]
    fn ordinary_names_that_look_close_are_not_hidden() {
        for name in [".vscodeignore", ".github", ".gitignore", ".gitattributes", "vscode", "git", "a.code-workspace.md"] {
            assert!(!is_hidden(name), "{name}");
        }
    }

    #[test]
    fn a_path_cannot_leave_the_project_or_name_the_whole_of_it_for_a_change() {
        for path in ["../x", "a/../../x", "/etc/passwd/../x", "a\nb", "a\u{1b}b"] {
            let reply = ask(json!({"id": 1, "op": "read", "path": path}), |argv, _| {
                panic!("{path:?} reached the sandbox: {argv:?}")
            });
            assert_eq!(reply["code"], "bad_path", "{path:?}");
        }
        for op in ["read", "write", "delete", "mkdir", "rename"] {
            let reply = ask(json!({"id": 1, "op": op, "path": "", "to": "x", "data": ""}), |argv, _| {
                panic!("{op} reached the sandbox: {argv:?}")
            });
            assert_eq!(reply["code"], "bad_path", "{op}");
        }
    }

    #[test]
    fn a_path_is_an_argument_and_never_part_of_a_script() {
        let reply = ask(json!({"id": 7, "op": "read", "path": "a;touch b/$(x).js"}), |argv, _| {
            assert!(!argv[2].contains("touch"), "{argv:?}");
            assert_eq!(argv.last().unwrap(), "/work/a;touch b/$(x).js");
            ok(b"hi")
        });
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["id"], 7);
        assert_eq!(reply["data"], to_hex(b"hi"));
    }

    #[test]
    fn a_listing_leaves_out_what_is_hidden_and_names_that_mislead() {
        let out = "41ed 96 100 /work/src\n81a4 5 200 /work/app.js\n41ed 1 1 /work/.vscode\n41ed 1 1 /work/.git\n81a4 1 1 /work/a\u{202e}b\n81a4 1 1 /elsewhere/x\nnot a stat line\n";
        let reply = ask(json!({"id": 1, "op": "list", "path": ""}), |_, _| ok(out.as_bytes()));
        let names: Vec<&str> = reply["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["src", "app.js"]);
        assert_eq!(reply["entries"][0]["kind"], "dir");
        assert_eq!(reply["entries"][1]["kind"], "file");
        assert_eq!(reply["entries"][1]["size"], 5);
        assert_eq!(reply["left_out"], 3);
    }

    /// Saving a file that was cut short would lose its end, so none comes back.
    #[test]
    fn a_file_over_the_limit_is_refused_rather_than_cut() {
        let reply = ask(json!({"id": 1, "op": "read", "path": "big"}), |_, _| Captured {
            code: Some(0),
            stdout: b"partial".to_vec(),
            stdout_cut: true,
            ..Captured::default()
        });
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["code"], "too_large");
        assert!(reply.get("data").is_none());
    }

    #[test]
    fn exit_codes_become_the_errors_the_editor_expects_and_a_stopped_read_fails() {
        for (code, want) in [(NOT_FOUND, "not_found"), (IS_DIR, "is_dir"), (EXISTS, "exists"), (1, "failed")] {
            let reply = ask(json!({"id": 1, "op": "read", "path": "x"}), |_, _| Captured {
                code: Some(code),
                ..Captured::default()
            });
            assert_eq!(reply["code"], want);
        }
        let reply = ask(json!({"id": 1, "op": "read", "path": "x"}), |_, _| Captured {
            code: None,
            timed_out: true,
            stdout: b"partial".to_vec(),
            ..Captured::default()
        });
        assert_eq!(reply["code"], "failed");
    }

    #[test]
    fn a_stopped_sandbox_is_reported_and_a_write_sends_exact_bytes() {
        let mut run = |_: &[String], _: Vec<u8>, _: usize, _: u64| -> Result<Captured, String> { Err("not running".to_owned()) };
        let reply = reply_to(&json!({"id": 1, "op": "stat", "path": "x"}).to_string(), &mut run);
        assert_eq!(reply["code"], "unavailable");

        let body = vec![0u8, 255, b'\n', b'a'];
        let reply = ask(
            json!({"id": 2, "op": "write", "path": "x", "data": to_hex(&body), "create": true, "overwrite": true}),
            |argv, input| {
                assert_eq!(input, vec![0u8, 255, b'\n', b'a']);
                assert_eq!(argv[4..], ["/work/x", "1", "1"]);
                ok(b"")
            },
        );
        assert_eq!(reply["ok"], true);
        let reply = ask(json!({"id": 3, "op": "write", "path": "x", "data": "zz"}), |argv, _| panic!("ran {argv:?}"));
        assert_eq!(reply["code"], "bad_request");
    }

    #[test]
    fn search_results_are_escaped_and_hidden_folders_stay_out() {
        let out = "./src/a.js:3:const x = 1;\u{1b}[2J\n./.VSCODE/s.json:1:x\n./b:c:9:y\nnonsense\n";
        let reply = ask(json!({"id": 1, "op": "search", "query": "x -e $(y)"}), |argv, _| {
            assert!(!argv[2].contains("$(y)"));
            assert_eq!(argv.last().unwrap(), "x -e $(y)");
            ok(out.as_bytes())
        });
        let found = reply["matches"].as_array().unwrap();
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0]["path"], "src/a.js");
        assert_eq!(found[0]["line"], 3);
        assert!(!found[0]["text"].as_str().unwrap().contains('\u{1b}'));
        assert_eq!(found[1]["path"], "b:c");
        for bad in ["", "a\nb"] {
            let reply = ask(json!({"id": 1, "op": "search", "query": bad}), |argv, _| panic!("ran {argv:?}"));
            assert_eq!(reply["code"], "bad_request");
        }
    }

    #[test]
    fn unknown_requests_and_bad_json_are_answered_not_fatal() {
        let reply = ask(json!({"id": 1, "op": "exec", "path": "x"}), |argv, _| panic!("ran {argv:?}"));
        assert_eq!(reply["code"], "bad_request");
        let mut run = |_: &[String], _: Vec<u8>, _: usize, _: u64| -> Result<Captured, String> { panic!("ran") };
        assert_eq!(reply_to("not json", &mut run)["code"], "bad_request");
    }

    #[test]
    fn hex_round_trips_and_refuses_junk() {
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(from_hex(&to_hex(&all)).unwrap(), all);
        for bad in ["a", "zz", "é0"] {
            assert_eq!(from_hex(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_profile_shuts_off_what_reaches_for_this_machine() {
        let binary = "/bin/the-cli";
        let settings = profile_settings(Path::new(binary));
        let mac = &settings["terminal.integrated.profiles.osx"];
        assert_eq!(mac[shell_profile()]["path"], binary);
        assert_eq!(mac[shell_profile()]["args"][0], "editor-shell");
        for local in ["bash", "zsh", "fish", "tmux", "pwsh"] {
            assert!(mac[local].is_null(), "{local} is still offered");
        }
        assert_eq!(settings["git.enabled"], false);
        assert_eq!(settings["task.allowAutomaticTasks"], "off");
        assert_eq!(settings["typescript.disableAutomaticTypeAcquisition"], true);
        assert_eq!(settings["terminal.integrated.defaultProfile.osx"], shell_profile().as_str());
        assert_eq!(settings["workbench.localHistory.enabled"], false);
        assert_eq!(settings["files.hotExit"], "off");
    }

    #[test]
    fn the_profile_drops_copies_of_files_kept_by_an_earlier_editor() {
        let dir = tempfile::tempdir().unwrap();
        let history = dir.path().join("data/User/History/-b2169e0");
        let backups = dir.path().join("data/Backups/x");
        std::fs::create_dir_all(&history).unwrap();
        std::fs::create_dir_all(&backups).unwrap();
        std::fs::write(history.join("mRPP.txt"), "TOKEN=fake").unwrap();
        prepare_profile(dir.path(), Path::new("/bin/the-cli")).unwrap();
        assert!(!dir.path().join("data/User/History").exists());
        assert!(!dir.path().join("data/Backups").exists());
        assert!(dir.path().join("data/User/settings.json").is_file());
    }

    #[test]
    fn the_profile_holds_the_extension_and_replaces_an_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join(format!("extensions/{}-0.0.1", extension_id()));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(dir.path().join("extensions/extensions.json"), "[]").unwrap();
        let (data, extensions) = prepare_profile(dir.path(), Path::new("/bin/the-cli")).unwrap();
        assert!(!old.exists());
        assert!(!extensions.join("extensions.json").exists());
        assert!(data.join("User/settings.json").is_file());
        let folder = extensions.join(format!("{}-{}", extension_id(), extension_version().unwrap()));
        assert!(folder.join("extension.js").is_file());
        let bridge: Value = serde_json::from_str(&std::fs::read_to_string(folder.join("bridge.json")).unwrap()).unwrap();
        assert_eq!(bridge["binary"], "/bin/the-cli");
    }

    #[test]
    fn the_folder_address_holds_the_project_in_a_form_a_url_cannot_change() {
        let uri = folder_uri(Path::new("/Users/A b/Proj"));
        assert!(uri.starts_with(&format!("{}-sandbox://", brand::CLI_BIN)));
        assert_eq!(uri.trim_end_matches('/').rsplit('/').next().unwrap(), to_hex(b"/Users/A b/Proj"));
    }
}
