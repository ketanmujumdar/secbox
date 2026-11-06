//! `diff`: what changed inside a sandbox since the snapshot it was given, with
//! the files that could act on this machine flagged first.
//!
//! The daemon does the work: the sandbox makes a bundle, and the host reviews
//! it without running anything from it. This prints the answer, and the
//! full-screen interface shows the same lines through [`lines`].

use std::path::PathBuf;
use std::time::Duration;

use clap::Args;
use proto::{DiffLineKind, DiffReport, FilePatch, ProjectArgs, Reply, Request};

use crate::blocked::Weight;
use crate::client::{CallError, DaemonClient};
use crate::ui::{esc, paint, Tone};
use crate::{exit, project_dir, say, tell, Deps};

/// The widest the path column gets before a long path just pushes the
/// message along.
const PATH_COLUMN: usize = 40;

#[derive(Args)]
pub struct DiffArgsCli {
    /// Project folder [default: the current folder]
    path: Option<PathBuf>,
    /// Also print the changed lines of each file
    #[arg(long)]
    patch: bool,
}

/// The changed lines of every file, for `--patch`. Text is escaped here,
/// whatever the daemon already did, and a file with no lines says why.
fn patch_lines(report: &DiffReport) -> Vec<(Option<Tone>, String)> {
    let mut out = Vec::new();
    let mut unread = 0;
    for file in &report.files {
        out.push((None, String::new()));
        let path = esc(&file.path);
        if file.patch.is_incomplete() {
            unread += 1;
        }
        let FilePatch::Text { hunks, added, removed, truncated } = &file.patch else {
            out.push((Some(Tone::Strong), path));
            let reason = file.patch.unread_reason().unwrap_or_default();
            out.push((Some(Tone::Warn), format!("  {reason}")));
            continue;
        };
        out.push((Some(Tone::Strong), format!("{path}  +{added} -{removed}")));
        if hunks.is_empty() {
            out.push((Some(Tone::Dim), format!("  {NO_LINES}")));
        }
        for hunk in hunks {
            let header = format!("@@ -{} +{} @@", hunk.old_start, hunk.new_start);
            out.push((Some(Tone::Dim), header));
            for line in &hunk.lines {
                let (sign, tone) = match line.kind {
                    DiffLineKind::Context => (' ', None),
                    DiffLineKind::Added => ('+', Some(Tone::Good)),
                    DiffLineKind::Removed => ('-', Some(Tone::Bad)),
                };
                let text = proto::text::escape_line(&line.text, usize::MAX);
                out.push((tone, format!("{sign}{text}")));
            }
        }
        if *truncated {
            out.push((Some(Tone::Warn), format!("  {CUT_SHORT}")));
        }
    }
    if unread > 0 {
        out.push((None, String::new()));
        out.push((
            Some(Tone::Bad),
            format!(
                "{unread} {} not shown in full. Treat what is missing as unread.",
                if unread == 1 { "file was" } else { "files were" }
            ),
        ));
    }
    out
}

/// A text change with no lines to show.
const NO_LINES: &str = "No lines changed. The file is empty, or only its type or permissions changed.";
const CUT_SHORT: &str = "Cut short at a limit. Treat the rest of this file as unread.";

/// What is always true of this comparison.
pub(crate) const HEURISTIC: &str = "Flags are a heuristic aid. Read the changes yourself before anything from this sandbox reaches your machine.";

/// Everything the report says, in the order a person should read it: a line
/// for each flag, then the changed files, then what is true of it all. Each
/// line is escaped here, whatever the daemon already did.
pub(crate) fn lines(report: &DiffReport) -> Vec<(Weight, String)> {
    let mut out = Vec::new();
    let base = report.baseline.get(..12).unwrap_or(&report.baseline);
    out.push((
        Weight::Plain,
        format!(
            "Compared with the snapshot this sandbox started from (commit {}).",
            esc(base)
        ),
    ));
    out.push((Weight::Plain, String::new()));

    let label = |flag: &proto::DiffFlag| {
        let path = flag.path.as_deref().unwrap_or("(the whole change)");
        match flag.line {
            Some(line) => format!("{}:{line}", esc(path)),
            None => esc(path),
        }
    };
    let width = report
        .flags
        .iter()
        .map(|flag| label(flag).chars().count())
        .max()
        .unwrap_or(0)
        .min(PATH_COLUMN);
    for flag in &report.flags {
        let mut message = esc(&flag.message);
        if flag.earlier_commit {
            message.push_str(" (in an earlier commit inside the sandbox)");
        }
        out.push((
            Weight::Alert,
            format!("FLAG  {:<width$}  {message}", label(flag)),
        ));
    }
    if !report.complete {
        out.push((
            Weight::Alert,
            "The review stopped at a limit, so there may be more to flag than this.".to_owned(),
        ));
    }
    if !report.flags.is_empty() {
        out.push((Weight::Plain, String::new()));
    }

    if report.files.is_empty() {
        out.push((Weight::Plain, "No files changed.".to_owned()));
    } else {
        let count = report.files.len();
        out.push((
            Weight::Plain,
            format!(
                "{count} {} changed:",
                if count == 1 { "file" } else { "files" }
            ),
        ));
        for file in &report.files {
            out.push((
                Weight::Plain,
                format!("  {:<9} {}", file.change.word(), esc(&file.path)),
            ));
        }
    }
    out.push((Weight::Plain, String::new()));
    if report.flags.is_empty() {
        out.push((Weight::Warn, format!("Nothing was flagged. {HEURISTIC}")));
    } else {
        let count = report.flags.len();
        out.push((
            Weight::Alert,
            format!(
                "{count} {} to look at. {HEURISTIC}",
                if count == 1 { "flag" } else { "flags" }
            ),
        ));
    }
    out.push((
        Weight::Warn,
        "Nothing was copied out of the sandbox. Export is not built yet.".to_owned(),
    ));
    out
}

/// Asks the daemon once, with a limit, for a caller that cannot wait for ever.
pub(crate) fn ask(
    client: &dyn DaemonClient,
    project: &str,
    limit: Duration,
) -> Result<DiffReport, CallError> {
    match client.stream(
        Request::Diff(ProjectArgs {
            project: project.to_owned(),
        }),
        &mut |_, _| true,
        Some(limit),
    ) {
        Ok(Reply::Diff(report)) => Ok(report),
        Ok(_) => Err(CallError::Transport(
            "the daemon answered something else".to_owned(),
        )),
        Err(error) => Err(error),
    }
}

/// The exit code a report earns: flags are worth a code of their own, so a
/// script can stop on them.
pub(crate) fn code(report: &DiffReport) -> i32 {
    if report.flagged() || !report.complete {
        exit::FLAGGED
    } else {
        exit::OK
    }
}

pub fn run(args: &DiffArgsCli, deps: &mut Deps) -> i32 {
    let root = match project_dir(args.path.as_deref(), deps) {
        Ok(root) => root,
        Err(reason) => {
            tell!(deps, "diff: {reason}");
            return exit::FAILED;
        }
    };
    let client = deps.daemon;
    let color = deps.color;
    let result = client.call(
        Request::Diff(ProjectArgs {
            project: root.to_string_lossy().into_owned(),
        }),
        &mut |step, detail| {
            let line = match detail {
                Some(detail) => format!("{step}: {detail}"),
                None => step.to_owned(),
            };
            say!(deps, "{}  {}", paint(color, Tone::Dim, "..."), esc(&line));
        },
    );
    let report = match result {
        Ok(Reply::Diff(report)) => report,
        Ok(_) => {
            tell!(deps, "diff: the daemon answered something else.");
            return exit::FAILED;
        }
        Err(error) => {
            tell!(deps, "diff: {}", error.for_terminal());
            return exit::FAILED;
        }
    };
    for (weight, text) in lines(&report) {
        let tone = match weight {
            Weight::Plain => None,
            Weight::Warn => Some(Tone::Warn),
            Weight::Alert => Some(Tone::Bad),
        };
        let text = match tone {
            Some(tone) => paint(color, tone, &text),
            None => text,
        };
        say!(deps, "{text}");
    }
    if args.patch {
        for (tone, text) in patch_lines(&report) {
            let text = match tone {
                Some(tone) => paint(color, tone, &text),
                None => text,
            };
            say!(deps, "{text}");
        }
    }
    code(&report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::{DiffFile, DiffFlag, FileChange, OutputStream};

    fn report(flags: Vec<DiffFlag>) -> DiffReport {
        DiffReport {
            project_id: "p".to_owned(),
            baseline: "1f2a12002068e9415f0b23be1962202c00fb97dd".to_owned(),
            flags,
            files: vec![
                DiffFile {
                    path: ".claude/settings.json".to_owned(),
                    change: FileChange::Added,
                    patch: proto::FilePatch::NotIncluded,
                },
                DiffFile {
                    path: "app.js".to_owned(),
                    change: FileChange::Modified,
                    patch: proto::FilePatch::NotIncluded,
                },
            ],
            commits: 1,
            complete: true,
        }
    }

    #[test]
    fn a_patch_prints_its_lines_and_says_what_is_unread() {
        let mut report = report(Vec::new());
        report.files[0].patch = FilePatch::Withheld;
        report.files[1].patch = FilePatch::Text {
            hunks: vec![proto::DiffHunk {
                old_start: 1,
                new_start: 1,
                lines: vec![
                    proto::DiffLine {
                        kind: DiffLineKind::Removed,
                        text: "old".to_owned(),
                    },
                    proto::DiffLine {
                        kind: DiffLineKind::Added,
                        text: "\tnew\u{1b}[2J".to_owned(),
                    },
                ],
            }],
            added: 1,
            removed: 1,
            truncated: false,
        };
        let text: Vec<String> = patch_lines(&report).into_iter().map(|(_, t)| t).collect();
        assert!(text.contains(&"app.js  +1 -1".to_owned()), "{text:?}");
        assert!(text.contains(&"@@ -1 +1 @@".to_owned()), "{text:?}");
        assert!(text.contains(&"-old".to_owned()), "{text:?}");
        // The tab stays, and the escape cannot reach the terminal.
        assert!(text.contains(&"+\tnew\\u{1b}[2J".to_owned()), "{text:?}");
        assert!(text.iter().any(|t| t.contains("treat this file as unread")), "{text:?}");
        assert!(text.last().unwrap().starts_with("1 file was not shown in full"), "{text:?}");
    }

    fn hook() -> DiffFlag {
        DiffFlag {
            kind: "agent_hook".to_owned(),
            path: Some(".claude/settings.json".to_owned()),
            line: None,
            message: "adds a hook for an AI coding tool, which would run on your machine"
                .to_owned(),
            earlier_commit: false,
        }
    }

    struct Answer(Result<DiffReport, fn() -> CallError>);

    impl DaemonClient for Answer {
        fn call(
            &self,
            request: Request,
            progress: &mut dyn FnMut(&str, Option<&str>),
        ) -> Result<Reply, CallError> {
            assert!(matches!(request, Request::Diff(_)));
            progress("bundling inside the sandbox", None);
            match &self.0 {
                Ok(report) => Ok(Reply::Diff(report.clone())),
                Err(make) => Err(make()),
            }
        }

        fn stream(
            &self,
            request: Request,
            _output: &mut dyn FnMut(OutputStream, &str) -> bool,
            limit: Option<Duration>,
        ) -> Result<Reply, CallError> {
            assert!(limit.is_some());
            self.call(request, &mut |_, _| {})
        }
    }

    fn run_with(answer: Answer) -> (i32, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let env = |_: &str| None;
        // As `with_test_deps`, but keeping the output to read.
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut input = std::io::Cursor::new(Vec::new());
        let store = |_: &secrets::ProjectId| -> Box<dyn secrets::SecretStore> { panic!() };
        let prompt = |_: &str| -> std::io::Result<secrets::SecretValue> { panic!() };
        let host = |_: &std::path::Path| -> crate::HostInfo { panic!() };
        let code = run(
            &DiffArgsCli { path: None, patch: false },
            &mut Deps {
                out: &mut out,
                err: &mut err,
                input: &mut input,
                color: false,
                terminal: false,
                cwd: dir.path().to_path_buf(),
                env: &env,
                store: &store,
                prompt: &prompt,
                host: &host,
                daemon: &answer,
            },
        );
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn flags_come_first_and_earn_their_own_exit_code() {
        let (code, out, _) = run_with(Answer(Ok(report(vec![hook()]))));
        assert_eq!(code, exit::FLAGGED);
        let flag_at = out
            .find("FLAG  .claude/settings.json  adds a hook")
            .unwrap();
        let files_at = out.find("2 files changed").unwrap();
        assert!(flag_at < files_at, "{out}");
        assert!(out.contains("  added     .claude/settings.json"), "{out}");
        assert!(out.contains("  modified  app.js"), "{out}");
        assert!(out.contains("Export is not built yet"), "{out}");
        assert!(out.contains("commit 1f2a12002068)"), "{out}");
    }

    #[test]
    fn a_clean_change_exits_zero_and_still_says_flags_are_a_heuristic() {
        let (code, out, _) = run_with(Answer(Ok(report(Vec::new()))));
        assert_eq!(code, exit::OK);
        assert!(!out.contains("FLAG"), "{out}");
        assert!(out.contains(HEURISTIC), "{out}");
    }

    #[test]
    fn an_incomplete_review_is_never_a_clean_one() {
        let mut incomplete = report(Vec::new());
        incomplete.complete = false;
        assert_eq!(code(&incomplete), exit::FLAGGED);
        assert!(lines(&incomplete)
            .iter()
            .any(|(w, t)| *w == Weight::Alert && t.contains("stopped at a limit")));
    }

    #[test]
    fn a_refusal_is_printed_escaped_and_fails() {
        let (code, _, err) = run_with(Answer(Err(|| {
            CallError::Refused(proto::ApiError::new(
                proto::ErrorCode::WrongState,
                "the sandbox is not running",
            ))
        })));
        assert_eq!(code, exit::FAILED);
        assert!(err.contains("diff: the sandbox is not running"), "{err}");
    }

    /// The daemon escapes everything, and this escapes it again, so a daemon
    /// with a bug still cannot reach the terminal.
    #[test]
    fn hostile_text_is_escaped_even_if_the_daemon_missed_it() {
        let mut flag = hook();
        flag.path = Some("evil\u{1b}[2J".to_owned());
        flag.message = "x\u{202e}y".to_owned();
        flag.earlier_commit = true;
        let mut hostile = report(vec![flag]);
        hostile.files[0].path = "a\u{1b}]0;title\u{7}".to_owned();
        for (_, text) in lines(&hostile) {
            assert!(
                !text.contains('\u{1b}') && !text.contains('\u{202e}'),
                "{text:?}"
            );
        }
        assert!(lines(&hostile)
            .iter()
            .any(|(_, t)| t.contains("earlier commit inside the sandbox")));
    }

    #[test]
    fn a_secret_flag_shows_its_line_but_never_a_value() {
        let flag = DiffFlag {
            kind: "likely_secret".to_owned(),
            path: Some("config.js".to_owned()),
            line: Some(12),
            message: "looks like it holds a secret (an AWS access key)".to_owned(),
            earlier_commit: false,
        };
        let text = lines(&report(vec![flag]))
            .into_iter()
            .map(|(_, t)| t)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("FLAG  config.js:12  looks like it holds a secret"),
            "{text}"
        );
    }
}
