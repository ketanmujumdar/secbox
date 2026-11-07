//! Comparing a sandbox with its snapshot, into the Changes tab.
//!
//! The comparison runs inside the sandbox and a large project can take minutes,
//! so it runs on a thread of its own and the interface keeps drawing and reading
//! keys. A client that cannot do that gets a call with a time limit.
//!
//! The Summary's lines are the ones `diff` prints, from the same function. Each
//! changed file also keeps its lines, or why it has none, from the same report.

use std::time::{Duration, Instant};

use proto::{DiffLineKind, DiffReport, FileChange, FilePatch, ProjectArgs, Reply, Request};

use crate::blocked::Weight;
use crate::client::{Background, CallError, DaemonClient, Event};
use crate::ui::esc;

use super::logs::failure;
use super::style::Style;
use super::view::{ChangedFile, Line, PatchRow, ReviewPanel, Row, RowState};

/// Bundling a large project inside the sandbox and reviewing it takes a while.
const COMPARE_LIMIT: Duration = Duration::from_secs(180);
/// A comparison this recent is not run again just because the tab was reopened.
pub const FRESH: Duration = Duration::from_secs(15);
const TAB: &str = "    ";

/// A comparison running on another thread. Dropping it ends it.
pub struct Comparing {
    call: Background,
    pub path: String,
    started: Instant,
}

impl Comparing {
    /// How it ended, once it has. This never waits.
    pub fn poll(&mut self) -> Option<Result<DiffReport, CallError>> {
        while let Some(event) = self.call.try_next() {
            if let Event::Done(answer) = event {
                return Some(match answer {
                    Ok(Reply::Diff(report)) => Ok(report),
                    Ok(_) => Err(CallError::Transport("the daemon answered something else".to_owned())),
                    Err(error) => Err(error),
                });
            }
        }
        (self.started.elapsed() > COMPARE_LIMIT).then(|| {
            Err(CallError::Transport(format!(
                "the comparison took over {} seconds, so it was stopped",
                COMPARE_LIMIT.as_secs()
            )))
        })
    }
}

/// Starts a comparison. `None` means this client cannot run one in the
/// background, and the caller should use [`refresh`].
pub fn begin(client: &dyn DaemonClient, path: &str) -> Option<Comparing> {
    client
        .background(Request::Diff(ProjectArgs { project: path.to_owned() }))
        .map(|call| Comparing { call, path: path.to_owned(), started: Instant::now() })
}

/// What to do about a comparison that was asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum Due {
    Start,
    /// One for this project is running, and the files have moved since it began.
    Wait,
    /// It is not worth running.
    Forget,
}

/// Decides whether a comparison asked for should run. `force` is for when it was
/// asked for by name, or the files are known to have moved. Otherwise it runs
/// only for a running sandbox, and not again straight after the last one.
pub fn due(panel: &ReviewPanel, row: &Row, running: Option<&str>, force: bool) -> Due {
    if running == Some(row.path.as_str()) {
        return if force { Due::Wait } else { Due::Forget };
    }
    if force {
        return Due::Start;
    }
    let fresh = panel.project == row.path && panel.compared_at.is_some_and(|at| at.elapsed() < FRESH);
    if row.state != RowState::Running || fresh {
        return Due::Forget;
    }
    Due::Start
}

/// Fills the panel for one project with a call that has a limit.
pub fn refresh(client: &dyn DaemonClient, panel: &mut ReviewPanel, path: &str) {
    let answer = crate::review::ask(client, path, COMPARE_LIMIT);
    fill(panel, path, answer);
}

/// Puts a comparison's answer in the panel. What it held is dropped first, so a
/// failure cannot leave an old comparison looking current. The file on show
/// stays on show, if it is still there.
pub fn fill(panel: &mut ReviewPanel, path: &str, answer: Result<DiffReport, CallError>) {
    let open = panel.open_path();
    let reading = panel.reading;
    let wanted = panel.wanted;
    *panel = ReviewPanel { project: path.to_owned(), wanted, ..ReviewPanel::default() };
    let report = match answer {
        Ok(report) => report,
        Err(error) => {
            panel.lines.push(failure(&error));
            return;
        }
    };
    panel.compared_at = Some(Instant::now());
    panel.files = Some(report.files.len());
    panel.flags = report.flags.len();
    panel.lines = crate::review::lines(&report)
        .into_iter()
        .map(|(weight, text)| {
            let style = match weight {
                Weight::Plain => Style::Window,
                Weight::Warn => Style::WindowWarn,
                Weight::Alert => Style::WindowAlert,
            };
            // Tabs or line breaks would break the frame; there are none after
            // escaping, but the panel does not rely on it.
            Line::styled(text.replace(['\t', '\n', '\r'], " "), style)
        })
        .collect();
    panel.changed = changed_files(&report);
    panel.top = top_lines(&report);
    if let Some(note) = unread_note(&report) {
        panel.lines.push(Line::styled(note, Style::WindowAlert));
    }
    // A flagged file comes first in the list, so it is what opens first.
    panel.chosen = open
        .and_then(|open| panel.changed.iter().position(|file| file.path == open))
        .map_or(usize::from(!panel.changed.is_empty()), |at| at + 1);
    panel.reading = reading && panel.chosen > 0;
}

/// The same words as the desktop app, for the same count.
fn unread_note(report: &DiffReport) -> Option<String> {
    let unread = report.files.iter().filter(|file| file.patch.is_incomplete()).count();
    (unread > 0).then(|| {
        format!(
            "{unread} {} not shown in full. Treat what is missing as unread.",
            if unread == 1 { "file was" } else { "files were" }
        )
    })
}

/// What has to stay in view whichever file is open.
fn top_lines(report: &DiffReport) -> Vec<Line> {
    let mut top = Vec::new();
    if !report.flags.is_empty() {
        let count = report.flags.len();
        top.push(Line::styled(
            format!(
                "{count} {} to look at. Flags are a heuristic aid: read the changes yourself.",
                if count == 1 { "flag" } else { "flags" }
            ),
            Style::WindowAlert,
        ));
    }
    if !report.complete {
        top.push(Line::styled(
            "The review stopped at a limit, so there may be more to flag than this.",
            Style::WindowAlert,
        ));
    }
    if let Some(note) = unread_note(report) {
        top.push(Line::styled(note, Style::WindowAlert));
    }
    top
}

/// Flagged files first, as flags come first everywhere else, then path order.
fn changed_files(report: &DiffReport) -> Vec<ChangedFile> {
    let mut files: Vec<ChangedFile> = report
        .files
        .iter()
        .map(|file| {
            let flags: Vec<&proto::DiffFlag> = report
                .flags
                .iter()
                .filter(|flag| flag.path.as_deref() == Some(file.path.as_str()))
                .collect();
            ChangedFile {
                letter: match file.change {
                    FileChange::Added => 'A',
                    FileChange::Modified => 'M',
                    FileChange::Deleted => 'D',
                    FileChange::TypeChanged => 'T',
                },
                path: esc(&file.path),
                flagged: !flags.is_empty(),
                stat: match &file.patch {
                    FilePatch::Text { added, removed, .. } => format!("+{added} -{removed}"),
                    _ => "not shown".to_owned(),
                },
                rows: file_rows(&file.patch, &flags),
            }
        })
        .collect();
    files.sort_by_key(|file| !file.flagged);
    files
}

fn note_row(text: impl Into<String>, style: Style) -> PatchRow {
    PatchRow { old: None, new: None, sign: None, text: text.into(), style }
}

/// One file's flags, then its lines or why there are none. Everything in here
/// came out of a sandbox, so it is escaped here whatever the daemon already did.
fn file_rows(patch: &FilePatch, flags: &[&proto::DiffFlag]) -> Vec<PatchRow> {
    let mut rows: Vec<PatchRow> = Vec::new();
    for flag in flags {
        let at = flag.line.map_or_else(String::new, |line| format!(" (line {line})"));
        let earlier = if flag.earlier_commit { " (in an earlier commit inside the sandbox)" } else { "" };
        rows.push(note_row(format!("FLAG{at}: {}{earlier}", esc(&flag.message)), Style::WindowAlert));
    }
    if let Some(reason) = patch.unread_reason() {
        rows.push(note_row(reason, Style::WindowWarn));
        return rows;
    }
    let FilePatch::Text { hunks, truncated, .. } = patch else {
        return rows;
    };
    if hunks.is_empty() {
        rows.push(note_row(crate::review::NO_LINES, Style::WindowDim));
    }
    for hunk in hunks {
        let (mut old, mut new) = (hunk.old_start, hunk.new_start);
        rows.push(note_row(format!("From line {}", if new > 0 { new } else { old }), Style::WindowDim));
        for line in &hunk.lines {
            let text = proto::text::escape_line(&line.text, usize::MAX).replace('\t', TAB);
            let (shown_old, shown_new, sign, style) = match line.kind {
                DiffLineKind::Context => (Some(old), Some(new), ' ', Style::Window),
                DiffLineKind::Added => (None, Some(new), '+', Style::WindowGood),
                DiffLineKind::Removed => (Some(old), None, '-', Style::WindowAlert),
            };
            rows.push(PatchRow { old: shown_old, new: shown_new, sign: Some(sign), text, style });
            old += u64::from(line.kind != DiffLineKind::Added);
            new += u64::from(line.kind != DiffLineKind::Removed);
        }
    }
    if *truncated {
        rows.push(note_row(crate::review::CUT_SHORT, Style::WindowWarn));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::CallError;
    use proto::{
        ApiError, DiffFile, DiffFlag, DiffHunk, DiffLine, ErrorCode, OutputStream,
        Request,
    };
    use std::cell::Cell;

    struct FakeDaemon {
        answer: fn() -> Result<Reply, CallError>,
        limited: Cell<bool>,
    }

    impl DaemonClient for FakeDaemon {
        fn call(
            &self,
            _request: Request,
            _progress: &mut dyn FnMut(&str, Option<&str>),
        ) -> Result<Reply, CallError> {
            panic!("the panel must use a call with a time limit");
        }

        fn stream(
            &self,
            request: Request,
            _output: &mut dyn FnMut(OutputStream, &str) -> bool,
            limit: Option<Duration>,
        ) -> Result<Reply, CallError> {
            assert!(matches!(request, Request::Diff(_)));
            self.limited.set(limit.is_some());
            (self.answer)()
        }
    }

    fn filled(answer: fn() -> Result<Reply, CallError>) -> ReviewPanel {
        let daemon = FakeDaemon {
            answer,
            limited: Cell::new(false),
        };
        let mut panel = ReviewPanel {
            project: "/p".to_owned(),
            lines: vec![Line::plain("left over")],
            scroll: 4,
            files: Some(9),
            flags: 9,
            ..ReviewPanel::default()
        };
        refresh(&daemon, &mut panel, "/p");
        assert!(
            daemon.limited.get(),
            "the panel must always set a time limit"
        );
        assert_eq!(panel.scroll, 0);
        assert!(!panel.lines.iter().any(|l| l.text == "left over"));
        panel
    }

    #[test]
    fn flags_come_first_in_alert_colour() {
        let panel = filled(|| {
            Ok(Reply::Diff(DiffReport {
                project_id: "p".to_owned(),
                baseline: "a".repeat(40),
                flags: vec![DiffFlag {
                    kind: "agent_hook".to_owned(),
                    path: Some(".claude/settings.json".to_owned()),
                    line: None,
                    message: "adds a hook for an AI coding tool, which would run on your machine"
                        .to_owned(),
                    earlier_commit: false,
                }],
                files: vec![DiffFile {
                    path: ".claude/settings.json".to_owned(),
                    change: FileChange::Added,
                    patch: proto::FilePatch::NotIncluded,
                }],
                commits: 1,
                complete: true,
            }))
        });
        let flag = panel
            .lines
            .iter()
            .position(|l| l.text.starts_with("FLAG"))
            .unwrap();
        let file = panel
            .lines
            .iter()
            .position(|l| l.text.contains("added"))
            .unwrap();
        assert!(flag < file);
        assert_eq!(panel.lines[flag].style, Style::WindowAlert);
        // The counts the Changes tab shows come from the same report.
        assert_eq!((panel.files, panel.flags), (Some(1), 1));
    }

    #[test]
    fn a_refusal_is_shown_in_place_of_the_list() {
        let panel = filled(|| {
            Err(CallError::Refused(ApiError::new(
                ErrorCode::WrongState,
                "the sandbox is not running",
            )))
        });
        assert_eq!(panel.lines.len(), 1);
        assert_eq!(panel.lines[0].style, Style::WindowAlert);
        assert!(panel.lines[0].text.contains("not running"));
        // An old count is not left looking current.
        assert_eq!((panel.files, panel.flags), (None, 0));
    }

    fn report_with(files: Vec<DiffFile>, flags: Vec<DiffFlag>) -> DiffReport {
        DiffReport {
            project_id: "p".to_owned(),
            baseline: "a".repeat(40),
            flags,
            files,
            commits: 1,
            complete: true,
        }
    }

    fn text_patch(lines: Vec<(DiffLineKind, &str)>, truncated: bool) -> FilePatch {
        FilePatch::Text {
            hunks: vec![DiffHunk {
                old_start: 12,
                new_start: 12,
                lines: lines
                    .into_iter()
                    .map(|(kind, text)| DiffLine { kind, text: text.to_owned() })
                    .collect(),
            }],
            added: 1,
            removed: 1,
            truncated,
        }
    }

    fn file(path: &str, patch: FilePatch) -> DiffFile {
        DiffFile { path: path.to_owned(), change: FileChange::Modified, patch }
    }

    fn flag(path: &str) -> DiffFlag {
        DiffFlag {
            kind: "agent_hook".to_owned(),
            path: Some(path.to_owned()),
            line: Some(3),
            message: "adds a hook".to_owned(),
            earlier_commit: false,
        }
    }

    #[test]
    fn a_flagged_file_opens_first_with_its_flags_over_its_true_line_numbers() {
        let mut panel = ReviewPanel::default();
        let patch = text_patch(
            vec![
                (DiffLineKind::Context, "same"),
                (DiffLineKind::Removed, "old"),
                (DiffLineKind::Added, "new"),
                (DiffLineKind::Context, "after"),
            ],
            false,
        );
        let report = report_with(
            vec![file("a.js", patch.clone()), file("z/.claude/settings.json", patch)],
            vec![flag("z/.claude/settings.json")],
        );
        fill(&mut panel, "/p", Ok(report));
        assert_eq!(panel.changed[0].path, "z/.claude/settings.json");
        assert!(panel.changed[0].flagged && !panel.changed[1].flagged);
        assert_eq!(panel.chosen, 1);
        assert_eq!(panel.changed[0].stat, "+1 -1");
        let rows = &panel.changed[0].rows;
        assert!(rows[0].text.starts_with("FLAG (line 3): adds a hook"));
        assert_eq!(rows[0].style, Style::WindowAlert);
        assert_eq!(rows[1].text, "From line 12");
        let numbers: Vec<_> = rows[2..].iter().map(|r| (r.old, r.new, r.sign)).collect();
        assert_eq!(
            numbers,
            [
                (Some(12), Some(12), Some(' ')),
                (Some(13), None, Some('-')),
                (None, Some(13), Some('+')),
                (Some(14), Some(14), Some(' ')),
            ]
        );
        assert_eq!(rows[3].style, Style::WindowAlert);
        assert_eq!(rows[4].style, Style::WindowGood);
    }

    #[test]
    fn hostile_lines_are_escaped_and_tabs_kept_out_of_the_frame() {
        let mut panel = ReviewPanel::default();
        let patch = text_patch(vec![(DiffLineKind::Added, "\tgo\u{1b}[2J\u{202e}")], false);
        fill(&mut panel, "/p", Ok(report_with(vec![file("a\u{1b}[31m", patch)], Vec::new())));
        let row = panel.changed[0].rows.last().unwrap();
        assert_eq!(row.text, "    go\\u{1b}[2J\\u{202e}");
        assert_eq!(panel.changed[0].path, "a\\u{1b}[31m");
    }

    #[test]
    fn what_is_not_shown_says_why_and_is_counted_in_the_desktops_words() {
        let mut panel = ReviewPanel::default();
        let report = report_with(
            vec![
                file("cut.js", text_patch(vec![(DiffLineKind::Added, "x")], true)),
                file("key.js", FilePatch::Withheld),
                file("ok.js", text_patch(vec![(DiffLineKind::Added, "x")], false)),
            ],
            Vec::new(),
        );
        fill(&mut panel, "/p", Ok(report));
        let said = "2 files were not shown in full. Treat what is missing as unread.";
        assert!(panel.top.iter().any(|l| l.text == said), "{:?}", panel.top);
        assert!(panel.lines.iter().any(|l| l.text == said));
        assert!(panel.changed[0].rows.last().unwrap().text.contains("Cut short"));
        assert_eq!(panel.changed[1].stat, "not shown");
        assert!(panel.changed[1].rows[0].text.contains("treat this file as unread"));
        let one = report_with(vec![file("key.js", FilePatch::Binary)], Vec::new());
        fill(&mut panel, "/p", Ok(one));
        assert!(panel.top[0].text.starts_with("1 file was not shown in full"));
    }

    #[test]
    fn comparing_again_keeps_the_file_on_show_and_a_failure_keeps_nothing() {
        let mut panel = ReviewPanel::default();
        let make = || {
            report_with(
                vec![
                    file("a.js", FilePatch::Binary),
                    file("b.js", FilePatch::Binary),
                ],
                Vec::new(),
            )
        };
        fill(&mut panel, "/p", Ok(make()));
        panel.move_file(1);
        panel.reading = true;
        assert_eq!(panel.open_path().as_deref(), Some("b.js"));
        fill(&mut panel, "/p", Ok(make()));
        assert_eq!(panel.open_path().as_deref(), Some("b.js"));
        assert!(panel.reading);
        fill(
            &mut panel,
            "/p",
            Err(CallError::Transport("gone".to_owned())),
        );
        assert!(panel.changed.is_empty() && panel.compared_at.is_none());
        assert_eq!(panel.lines.len(), 1);
    }

    fn row(state: RowState) -> Row {
        Row { name: "p".to_owned(), path: "/p".to_owned(), state }
    }

    #[test]
    fn it_compares_by_itself_only_for_a_running_sandbox_and_not_straight_after_the_last() {
        let mut panel = ReviewPanel { project: "/p".to_owned(), ..ReviewPanel::default() };
        let running = row(RowState::Running);
        assert_eq!(due(&panel, &running, None, false), Due::Start);
        for state in [RowState::Stopped, RowState::None, RowState::Unknown] {
            assert_eq!(due(&panel, &row(state), None, false), Due::Forget);
        }
        panel.compared_at = Some(Instant::now());
        assert_eq!(due(&panel, &running, None, false), Due::Forget);
        // r, and what follows a Git command, are not held back.
        assert_eq!(due(&panel, &running, None, true), Due::Start);
        // Another project's fresh answer says nothing about this one.
        panel.project = "/other".to_owned();
        assert_eq!(due(&panel, &running, None, false), Due::Start);
    }

    #[test]
    fn a_comparison_already_running_is_not_doubled() {
        let panel = ReviewPanel::default();
        let running = row(RowState::Running);
        assert_eq!(due(&panel, &running, Some("/p"), false), Due::Forget);
        // The files moved after it began, so another follows it.
        assert_eq!(due(&panel, &running, Some("/p"), true), Due::Wait);
        assert_eq!(due(&panel, &running, Some("/other"), false), Due::Start);
    }

    #[test]
    fn a_background_comparison_hands_over_its_answer_and_says_when_it_ends_early() {
        let (sender, events) = std::sync::mpsc::sync_channel(4);
        let mut live = Comparing {
            call: Background::new(events, || {}),
            path: "/p".to_owned(),
            started: Instant::now(),
        };
        assert!(live.poll().is_none());
        sender.send(Event::Output(OutputStream::Stdout, "x".to_owned())).unwrap();
        assert!(live.poll().is_none());
        sender.send(Event::Done(Ok(Reply::Diff(report_with(Vec::new(), Vec::new()))))).unwrap();
        assert!(live.poll().unwrap().is_ok());

        let (sender, events) = std::sync::mpsc::sync_channel::<Event>(1);
        let mut live = Comparing {
            call: Background::new(events, || {}),
            path: "/p".to_owned(),
            started: Instant::now(),
        };
        drop(sender);
        assert!(live.poll().unwrap().is_err());
    }
}
