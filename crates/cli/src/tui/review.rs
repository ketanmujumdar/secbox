//! Comparing a sandbox with its snapshot, into the Changes tab.
//!
//! The lines are the ones `diff` prints, from the same function. The call has
//! a time limit, because the interface cannot draw or read a key while it
//! waits, and a comparison runs inside the sandbox.

use std::time::Duration;

use crate::blocked::Weight;
use crate::client::DaemonClient;

use super::logs::failure;
use super::style::Style;
use super::view::{Line, ReviewPanel};

/// Bundling a large project inside the sandbox and reviewing it takes a while.
const COMPARE_LIMIT: Duration = Duration::from_secs(180);

/// Fills the panel for one project. What it held is dropped first, so a
/// failure cannot leave an old comparison looking current.
pub fn refresh(client: &dyn DaemonClient, panel: &mut ReviewPanel, path: &str) {
    panel.project = path.to_owned();
    panel.lines.clear();
    panel.scroll = 0;
    panel.files = None;
    panel.flags = 0;
    match crate::review::ask(client, path, COMPARE_LIMIT) {
        Ok(report) => {
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
                    // Tabs or line breaks would break the frame; there are
                    // none after escaping, but the panel does not rely on it.
                    Line::styled(text.replace(['\t', '\n', '\r'], " "), style)
                })
                .collect();
        }
        Err(error) => panel.lines.push(failure(&error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::CallError;
    use proto::{
        ApiError, DiffFile, DiffFlag, DiffReport, ErrorCode, FileChange, OutputStream, Reply,
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
}
