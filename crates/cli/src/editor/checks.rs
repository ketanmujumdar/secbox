//! The named editor safety checks: a stable id, a title, and a verdict each.
//!
//! Every file read here is treated as hostile. A file that cannot be read or
//! parsed is never a pass: a project file that cannot be checked fails, and a
//! user file that cannot be checked is skipped and said to be skipped.
//!
//! ## Can a project override the user-level settings?
//!
//! No, and this was confirmed from the documentation rather than assumed.
//! `task.allowAutomaticTasks` and the five `security.workspace.trust.*`
//! settings are all registered with `ConfigurationScope.APPLICATION`
//! (`src/vs/workbench/contrib/tasks/browser/task.contribution.ts` and
//! `src/vs/workbench/contrib/workspace/browser/workspace.contribution.ts`),
//! and the editor's own documentation says an application-scoped setting holds
//! "Settings that apply to all instances of VS Code and can only be configured
//! in user settings"
//! <https://code.visualstudio.com/api/references/contribution-points>. The
//! settings guide says the same from the other side: "application-wide
//! settings related to updates and security can not be overridden by Workspace
//! settings" <https://code.visualstudio.com/docs/configure/settings>.
//!
//! So a value in `.vscode/settings.json` or a `*.code-workspace` file does not
//! change the user's choice. It is still reported, and still fails, because a
//! repo that ships one is asking for the safety to be off and the same file
//! will be applied by a fork or a future release that scopes it differently.
//! It is reported as an intent, not as an effective override, and the report
//! says so.
//!
//! The automatic-task setting is the one that matters for opening a folder:
//! it "controls whether tasks with "runOn": "folderOpen" are allowed to run
//! automatically when you open a workspace", its default is `off`, and
//! "Automatic tasks never run in an untrusted workspace, regardless of this
//! setting" <https://code.visualstudio.com/docs/editor/tasks>.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::jsonc::{self, Map, Source};
use super::paths::{user_settings, Editor, Platform};
use crate::ui::{esc, esc_path};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    Pass,
    Fail,
    /// The editor is not installed, or the file this check reads is absent.
    NotApplicable,
    /// The check could not run. Never counted as a pass.
    Skipped,
    /// Turned off by the user, at user level.
    Off,
}

impl Verdict {
    pub fn word(self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Fail => "FAIL",
            Verdict::NotApplicable => "n/a",
            Verdict::Skipped => "skipped",
            Verdict::Off => "off",
        }
    }
}

/// What one check looks at. Ids are stable: they are what the user turns on
/// and off, and what an override is recorded against.
pub struct CheckDef {
    pub id: &'static str,
    pub title: &'static str,
    pub kind: Kind,
}

#[derive(Clone, Copy)]
pub enum Kind {
    /// User-level automatic tasks for one editor.
    AutoTasks(Editor),
    /// User-level Workspace Trust for one editor.
    Trust(Editor),
    /// Project tasks that ask to run when the folder opens.
    FolderOpenTasks,
    /// Project settings that change the user-level values above.
    SettingsOverride,
    /// Hooks the agent would run from the project.
    AgentHooks,
}

pub const CHECKS: &[CheckDef] = &[
    CheckDef {
        id: "vscode-automatic-tasks",
        title: "VS Code does not run automatic tasks",
        kind: Kind::AutoTasks(Editor::Code),
    },
    CheckDef {
        id: "vscode-insiders-automatic-tasks",
        title: "VS Code Insiders does not run automatic tasks",
        kind: Kind::AutoTasks(Editor::CodeInsiders),
    },
    CheckDef {
        id: "cursor-automatic-tasks",
        title: "Cursor does not run automatic tasks",
        kind: Kind::AutoTasks(Editor::Cursor),
    },
    CheckDef {
        id: "vscode-workspace-trust",
        title: "VS Code keeps Workspace Trust on",
        kind: Kind::Trust(Editor::Code),
    },
    CheckDef {
        id: "vscode-insiders-workspace-trust",
        title: "VS Code Insiders keeps Workspace Trust on",
        kind: Kind::Trust(Editor::CodeInsiders),
    },
    CheckDef {
        id: "cursor-workspace-trust",
        title: "Cursor keeps Workspace Trust on",
        kind: Kind::Trust(Editor::Cursor),
    },
    CheckDef {
        id: "project-folder-open-tasks",
        title: "No project task asks to run when the folder opens",
        kind: Kind::FolderOpenTasks,
    },
    CheckDef {
        id: "project-settings-override",
        title: "Project settings do not ask for these safeties to be off",
        kind: Kind::SettingsOverride,
    },
    CheckDef {
        id: "project-agent-hooks",
        title: "No agent hooks are set in the project",
        kind: Kind::AgentHooks,
    },
];

pub fn find(id: &str) -> Option<&'static CheckDef> {
    CHECKS.iter().find(|c| c.id == id)
}

/// A setting whose value can weaken the editor, and the value a fix writes.
pub struct WatchedKey {
    pub name: &'static str,
    /// The value that weakens the editor, written as JSON.
    pub weak: &'static str,
    /// The value `--fix` writes, written as JSON.
    pub safe: &'static str,
    pub why: &'static str,
}

pub const AUTO_TASK_KEYS: &[WatchedKey] = &[WatchedKey {
    name: "task.allowAutomaticTasks",
    weak: "\"on\"",
    safe: "\"off\"",
    why: "a task with runOn folderOpen would run as soon as the folder opens",
}];

pub const TRUST_KEYS: &[WatchedKey] = &[
    WatchedKey {
        name: "security.workspace.trust.enabled",
        weak: "false",
        safe: "true",
        why: "every folder would open with full powers, with no Restricted Mode",
    },
    WatchedKey {
        name: "security.workspace.trust.untrustedFiles",
        weak: "\"open\"",
        safe: "\"prompt\"",
        why: "files from elsewhere would join a trusted folder without asking",
    },
    WatchedKey {
        name: "security.workspace.trust.banner",
        weak: "\"never\"",
        safe: "\"untilDismissed\"",
        why: "Restricted Mode would be hidden, so a risky folder would look normal",
    },
];

/// Every watched key, for reading project files.
pub fn watched() -> impl Iterator<Item = &'static WatchedKey> {
    AUTO_TASK_KEYS.iter().chain(TRUST_KEYS.iter())
}

/// Where the checks look. Nothing here comes from a project file.
pub struct Context {
    pub root: PathBuf,
    pub home: PathBuf,
    pub xdg: Option<PathBuf>,
    pub platform: Platform,
}

/// One change `--fix` could make to a user settings file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    pub editor: Editor,
    pub settings: PathBuf,
    pub key: &'static str,
    pub value: &'static str,
}

pub struct Outcome {
    pub id: &'static str,
    pub title: &'static str,
    pub verdict: Verdict,
    /// Already escaped, ready to print.
    pub notes: Vec<String>,
    pub fixes: Vec<Fix>,
}

impl Outcome {
    fn new(def: &'static CheckDef, verdict: Verdict) -> Self {
        Self {
            id: def.id,
            title: def.title,
            verdict,
            notes: Vec::new(),
            fixes: Vec::new(),
        }
    }

    fn note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}

/// Runs the checks that are on. `editors` limits the user-level checks to the
/// editors this run cares about; the rest are left out of the report.
pub fn run(ctx: &Context, editors: &[Editor], is_on: &dyn Fn(&str) -> bool) -> Vec<Outcome> {
    let mut out = Vec::new();
    for def in CHECKS {
        if let Kind::AutoTasks(editor) | Kind::Trust(editor) = def.kind {
            if !editors.contains(&editor) {
                continue;
            }
        }
        if !is_on(def.id) {
            out.push(Outcome::new(def, Verdict::Off));
            continue;
        }
        out.push(match def.kind {
            Kind::AutoTasks(editor) => user_keys(ctx, def, editor, AUTO_TASK_KEYS),
            Kind::Trust(editor) => user_keys(ctx, def, editor, TRUST_KEYS),
            Kind::FolderOpenTasks => folder_open_tasks(ctx, def),
            Kind::SettingsOverride => settings_override(ctx, def),
            Kind::AgentHooks => agent_hooks(ctx, def),
        });
    }
    out
}

pub fn any_failed(outcomes: &[Outcome]) -> bool {
    outcomes.iter().any(|o| o.verdict == Verdict::Fail)
}

/// Reads one editor's user settings and judges a set of keys.
fn user_keys(
    ctx: &Context,
    def: &'static CheckDef,
    editor: Editor,
    keys: &'static [WatchedKey],
) -> Outcome {
    let places = user_settings(&ctx.home, ctx.xdg.as_deref(), ctx.platform, editor);
    let settings = match jsonc::read(&places.settings) {
        Source::Missing if !places.user_dir.is_dir() => {
            return Outcome::new(def, Verdict::NotApplicable)
                .note(format!("no user folder at {}", esc_path(&places.user_dir)));
        }
        Source::Missing => {
            return Outcome::new(def, Verdict::Pass).note(format!(
                "no settings file at {}, so the safe defaults apply",
                esc_path(&places.settings)
            ));
        }
        Source::NotRegular => {
            return Outcome::new(def, Verdict::Skipped).note(format!(
                "{} is not a regular file, so it was not read",
                esc_path(&places.settings)
            ));
        }
        Source::TooLarge => {
            return Outcome::new(def, Verdict::Skipped).note(format!(
                "{} is larger than {} bytes, so it was not read",
                esc_path(&places.settings),
                jsonc::MAX_BYTES
            ));
        }
        Source::Unreadable(reason) => {
            return Outcome::new(def, Verdict::Skipped).note(format!(
                "{} could not be read: {}",
                esc_path(&places.settings),
                esc(&reason)
            ));
        }
        Source::Text(text) => text,
    };
    let value = match jsonc::parse(&settings) {
        Ok(value) => value,
        Err(reason) => {
            return Outcome::new(def, Verdict::Skipped).note(format!(
                "{} could not be understood as JSON at {}",
                esc_path(&places.settings),
                esc(&reason)
            ));
        }
    };
    let Some(map) = value.as_object() else {
        return Outcome::new(def, Verdict::Skipped).note(format!(
            "{} does not hold a JSON object",
            esc_path(&places.settings)
        ));
    };
    let mut outcome = Outcome::new(def, Verdict::Pass);
    for key in keys {
        match judge(map, key) {
            Judged::Absent => {}
            Judged::Safe => outcome
                .notes
                .push(format!("{} is set and is safe", esc(key.name))),
            Judged::Weak => {
                outcome.verdict = Verdict::Fail;
                outcome.notes.push(format!(
                    "{} is {} in {}: {}",
                    esc(key.name),
                    esc(key.weak),
                    esc_path(&places.settings),
                    esc(key.why)
                ));
                outcome.fixes.push(Fix {
                    editor,
                    settings: places.settings.clone(),
                    key: key.name,
                    value: key.safe,
                });
            }
        }
    }
    if outcome.notes.is_empty() {
        outcome
            .notes
            .push("none of these settings are set, so the safe defaults apply".to_string());
    }
    outcome
}

enum Judged {
    Absent,
    Safe,
    Weak,
}

fn judge(map: &Map, key: &WatchedKey) -> Judged {
    let Some(found) = map.get(key.name) else {
        return Judged::Absent;
    };
    let weak: Value = serde_json::from_str(key.weak).unwrap_or(Value::Null);
    if *found == weak {
        Judged::Weak
    } else {
        Judged::Safe
    }
}

/// Loads a project file. Anything that stops it being understood is a reason
/// to fail, because an unchecked file is not a clean one.
enum Loaded {
    Absent,
    Problem(String),
    Value(Value),
}

fn load(path: &Path) -> Loaded {
    let text = match jsonc::read(path) {
        Source::Missing => return Loaded::Absent,
        Source::NotRegular => {
            return Loaded::Problem(format!(
                "{} is not a regular file, so it could not be checked",
                esc_path(path)
            ))
        }
        Source::TooLarge => {
            return Loaded::Problem(format!(
                "{} is larger than {} bytes, so it could not be checked",
                esc_path(path),
                jsonc::MAX_BYTES
            ))
        }
        Source::Unreadable(reason) => {
            return Loaded::Problem(format!(
                "{} could not be read: {}",
                esc_path(path),
                esc(&reason)
            ))
        }
        Source::Text(text) => text,
    };
    match jsonc::parse(&text) {
        Ok(value) => Loaded::Value(value),
        Err(reason) => Loaded::Problem(format!(
            "{} could not be understood as JSON at {}",
            esc_path(path),
            esc(&reason)
        )),
    }
}

/// The project files that can carry tasks or settings. Only the project root
/// and its `.vscode` folder are read, and the list is capped.
const MAX_WORKSPACE_FILES: usize = 20;

fn workspace_files(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "code-workspace"))
        .collect();
    found.sort();
    found.truncate(MAX_WORKSPACE_FILES);
    found
}

fn task_files(root: &Path) -> Vec<PathBuf> {
    let mut files = vec![root.join(".vscode").join("tasks.json")];
    files.extend(workspace_files(root));
    files
}

fn folder_open_tasks(ctx: &Context, def: &'static CheckDef) -> Outcome {
    let mut outcome = Outcome::new(def, Verdict::Pass);
    let mut looked = 0;
    for path in task_files(&ctx.root) {
        let value = match load(&path) {
            Loaded::Absent => continue,
            Loaded::Problem(reason) => {
                outcome.verdict = Verdict::Fail;
                outcome.notes.push(reason);
                continue;
            }
            Loaded::Value(value) => value,
        };
        looked += 1;
        let found = jsonc::find_objects(&value, &runs_on_folder_open);
        for label in found {
            outcome.verdict = Verdict::Fail;
            outcome.notes.push(format!(
                "{}: {} runs when the folder opens",
                esc_path(&path),
                esc(&label)
            ));
        }
    }
    if outcome.notes.is_empty() {
        outcome.notes.push(if looked == 0 {
            "no tasks file in the project".to_string()
        } else {
            "no task asks to run when the folder opens".to_string()
        });
    }
    outcome
}

/// A task object that asks to run on folder open. The schema puts `runOn`
/// inside `runOptions`; a stray `runOn` at the top of a task is treated the
/// same way rather than argued with. The options object itself is not a
/// match, so one task is reported once.
fn runs_on_folder_open(map: &Map, under: Option<&str>) -> bool {
    let nested = map
        .get("runOptions")
        .and_then(Value::as_object)
        .and_then(|o| o.get("runOn"))
        .and_then(Value::as_str);
    if nested == Some("folderOpen") {
        return true;
    }
    under != Some("runOptions") && map.get("runOn").and_then(Value::as_str) == Some("folderOpen")
}

fn settings_override(ctx: &Context, def: &'static CheckDef) -> Outcome {
    let mut outcome = Outcome::new(def, Verdict::Pass);
    let mut files = vec![ctx.root.join(".vscode").join("settings.json")];
    files.extend(workspace_files(&ctx.root));
    let mut looked = 0;
    for path in files {
        let value = match load(&path) {
            Loaded::Absent => continue,
            Loaded::Problem(reason) => {
                outcome.verdict = Verdict::Fail;
                outcome.notes.push(reason);
                continue;
            }
            Loaded::Value(value) => value,
        };
        looked += 1;
        // A workspace file keeps its settings under `settings`; a settings
        // file is the object itself.
        let holder = value
            .get("settings")
            .and_then(Value::as_object)
            .or_else(|| value.as_object());
        let Some(map) = holder else {
            continue;
        };
        for key in watched() {
            match judge(map, key) {
                Judged::Absent => {}
                Judged::Safe => outcome.notes.push(format!(
                    "{} sets {} to a safe value",
                    esc_path(&path),
                    esc(key.name)
                )),
                Judged::Weak => {
                    outcome.verdict = Verdict::Fail;
                    outcome.notes.push(format!(
                        "{} asks for {} to be {}: {}",
                        esc_path(&path),
                        esc(key.name),
                        esc(key.weak),
                        esc(key.why)
                    ));
                }
            }
        }
    }
    if outcome.notes.is_empty() {
        outcome.notes.push(if looked == 0 {
            "no project settings file".to_string()
        } else {
            "no project settings file asks for these safeties to be off".to_string()
        });
    }
    outcome
}

const AGENT_FILES: [&str; 2] = ["settings.json", "settings.local.json"];

fn agent_hooks(ctx: &Context, def: &'static CheckDef) -> Outcome {
    let mut outcome = Outcome::new(def, Verdict::Pass);
    let mut looked = 0;
    for name in AGENT_FILES {
        let path = ctx.root.join(".claude").join(name);
        let value = match load(&path) {
            Loaded::Absent => continue,
            Loaded::Problem(reason) => {
                outcome.verdict = Verdict::Fail;
                outcome.notes.push(reason);
                continue;
            }
            Loaded::Value(value) => value,
        };
        looked += 1;
        for line in hook_lines(&value) {
            outcome.verdict = Verdict::Fail;
            outcome
                .notes
                .push(format!("{}: {}", esc_path(&path), esc(&line)));
        }
    }
    if outcome.notes.is_empty() {
        outcome.notes.push(if looked == 0 {
            "no agent settings file in the project".to_string()
        } else {
            "the agent settings file sets no hooks".to_string()
        });
    }
    outcome
}

/// Describes each hook without running anything. Commands are trimmed, and
/// the caller escapes them before printing.
fn hook_lines(value: &Value) -> Vec<String> {
    let Some(hooks) = value.get("hooks").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for (event, entry) in hooks {
        let commands = jsonc::find_objects(entry, &|map: &Map, _: Option<&str>| {
            map.get("command").and_then(Value::as_str).is_some()
        });
        if commands.is_empty() {
            lines.push(format!("a hook on {} is set", jsonc::cut(event)));
            continue;
        }
        for command in commands {
            lines.push(format!("a hook on {} runs {command}", jsonc::cut(event)));
        }
    }
    lines.sort();
    lines
}
