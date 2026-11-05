//! Reading a project's logs into the Logs panel.
//!
//! Each call here has a time limit, because the interface cannot draw or read
//! a key while it waits. Only the daemon can read a sandbox, and it refuses
//! one that is not running, but this does not get that far: it asks for the
//! state first, so that it can say how to start one.

use std::collections::VecDeque;
use std::time::Duration;

use proto::text::{self, TerminalFilter};
use proto::{check_service_name, LogsArgs, ProjectArgs, Reply, Request, SandboxState, StatusReport};

use super::screen::cut;
use super::style::Style;
use super::view::{Line, LogsPanel};
use crate::client::{CallError, DaemonClient};

/// How long a status request may take.
const STATUS_LIMIT: Duration = Duration::from_secs(15);
/// How long reading logs may take. The daemon has its own, longer, limit.
const READ_LIMIT: Duration = Duration::from_secs(20);
/// How many recent lines to ask for from each service.
const TAIL_LINES: u32 = 200;
/// The most lines kept, however many a sandbox sends.
const KEPT_LINES: usize = 2000;
const KEPT_LINE_CHARS: usize = 1000;
const TAB: &str = "    ";

pub const HELP: &str = "Logs: [ ] choose a service or All, 5 reads again, arrows change project.";

pub fn status(client: &dyn DaemonClient, path: &str) -> Result<StatusReport, CallError> {
    match client.stream(
        Request::Status(ProjectArgs {
            project: path.to_owned(),
        }),
        &mut |_, _| true,
        Some(STATUS_LIMIT),
    ) {
        Ok(Reply::Status(report)) => Ok(report),
        Ok(_) => Err(CallError::Transport(
            "the daemon answered something else".to_owned(),
        )),
        Err(error) => Err(error),
    }
}

/// The names that can be asked for by name, in order and once each. A name
/// from inside the sandbox that fails the check would only be refused.
fn usable_services(report: &StatusReport) -> Vec<String> {
    let mut names: Vec<String> = report
        .services
        .iter()
        .map(|service| service.name.clone())
        .filter(|name| check_service_name(name).is_ok())
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

fn not_running_note(state: SandboxState, path: &str) -> String {
    let start = format!(
        "Press 2 to start it, or run: {} up {}",
        brand::CLI_BIN,
        text::escape(path)
    );
    match state {
        SandboxState::Stopped => format!("The sandbox is stopped. {start}"),
        SandboxState::None => format!("This project has no sandbox. {start}"),
        SandboxState::Unknown | SandboxState::Running => {
            "The sandbox is in a state this version does not understand.".to_owned()
        }
    }
}

/// Turns filtered output into lines that fit one row each. A tab would take
/// one cell in the grid but move the cursor on a real terminal.
#[derive(Default)]
struct Lines {
    lines: VecDeque<String>,
    open: String,
}

impl Lines {
    fn add(&mut self, text: &str) {
        for piece in text.split_inclusive('\n') {
            match piece.strip_suffix('\n') {
                Some(end) => {
                    self.append(end);
                    let line = std::mem::take(&mut self.open);
                    self.keep(line);
                }
                None => self.append(piece),
            }
        }
    }

    fn append(&mut self, text: &str) {
        // Bounded here too, since one line with no end could otherwise fill
        // memory. The cut below marks that it is not all there.
        let room = (KEPT_LINE_CHARS + 1).saturating_sub(self.open.chars().count());
        self.open
            .extend(text.replace('\t', TAB).chars().take(room));
    }

    fn keep(&mut self, line: String) {
        if self.lines.len() == KEPT_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(cut(&line, KEPT_LINE_CHARS));
    }

    fn finish(mut self) -> Vec<String> {
        if !self.open.is_empty() {
            let line = std::mem::take(&mut self.open);
            self.keep(line);
        }
        self.lines.into()
    }
}

/// Fills the panel for one project. The chosen service survives a refresh, and
/// is forgotten only if the project no longer has it.
pub fn refresh(client: &dyn DaemonClient, panel: &mut LogsPanel, path: &str) {
    panel.lines.clear();
    panel.note = None;
    let report = match status(client, path) {
        Ok(report) => report,
        Err(error) => {
            panel.note = Some(failure(&error));
            return;
        }
    };
    if report.sandbox != SandboxState::Running {
        panel.services.clear();
        panel.chosen = None;
        panel.note = Some(Line::styled(
            not_running_note(report.sandbox, path),
            Style::WindowWarn,
        ));
        return;
    }
    panel.services = usable_services(&report);
    if let Some(chosen) = &panel.chosen {
        if !panel.services.contains(chosen) {
            panel.chosen = None;
        }
    }

    // Filtered again here, as everything from the daemon is before it is shown.
    let mut stdout = TerminalFilter::new();
    let mut stderr = TerminalFilter::new();
    let mut lines = Lines::default();
    let result = client.stream(
        Request::Logs(LogsArgs {
            project: path.to_owned(),
            service: panel.chosen.clone(),
            tail: TAIL_LINES,
            follow: false,
        }),
        &mut |stream, text| {
            let filter = match stream {
                proto::OutputStream::Stdout => &mut stdout,
                proto::OutputStream::Stderr => &mut stderr,
            };
            lines.add(&filter.push(text.as_bytes()));
            true
        },
        Some(READ_LIMIT),
    );
    lines.add(&stdout.finish());
    lines.add(&stderr.finish());
    panel.lines = lines.finish();

    panel.note = match result {
        Ok(Reply::Logs(report)) => match report.code {
            Some(0) | None if panel.lines.is_empty() => {
                Some(Line::dim("Nothing has been logged yet."))
            }
            Some(0) | None => None,
            Some(code) => Some(Line::styled(
                format!("The log command inside the sandbox exited with {code}."),
                Style::WindowAlert,
            )),
        },
        Ok(_) => Some(Line::styled(
            "The daemon answered something else.",
            Style::WindowAlert,
        )),
        Err(error) => Some(failure(&error)),
    };
}

fn failure(error: &CallError) -> Line {
    let message = match error {
        CallError::TimedOut => "The daemon did not answer in time. Press 5 to try again.".to_owned(),
        other => text::escape(&other.message().replace('\n', " ")),
    };
    Line::styled(message, Style::WindowAlert)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::{ApiError, ErrorCode, LogsReport, OutputStream, ServiceState};
    use std::cell::RefCell;

    /// Answers the way the real daemon does: it refuses to read a sandbox that
    /// is not running, and refuses a bad service name.
    struct FakeDaemon {
        state: SandboxState,
        services: Vec<&'static str>,
        chunks: Vec<&'static [u8]>,
        requests: RefCell<Vec<Request>>,
    }

    impl FakeDaemon {
        fn new(state: SandboxState) -> Self {
            Self {
                state,
                services: vec!["web", "api", "web"],
                chunks: Vec::new(),
                requests: RefCell::new(Vec::new()),
            }
        }

        fn asked_for_logs(&self) -> bool {
            self.requests
                .borrow()
                .iter()
                .any(|r| matches!(r, Request::Logs(_)))
        }
    }

    impl DaemonClient for FakeDaemon {
        fn call(
            &self,
            request: Request,
            _progress: &mut dyn FnMut(&str, Option<&str>),
        ) -> Result<Reply, CallError> {
            self.stream(request, &mut |_, _| true, None)
        }

        fn stream(
            &self,
            request: Request,
            output: &mut dyn FnMut(OutputStream, &str) -> bool,
            _limit: Option<Duration>,
        ) -> Result<Reply, CallError> {
            self.requests.borrow_mut().push(request.clone());
            match request {
                Request::Status(_) => Ok(Reply::Status(StatusReport {
                    project_id: "p".to_owned(),
                    project_path: "/p".to_owned(),
                    sandbox: self.state,
                    services: self
                        .services
                        .iter()
                        .map(|name| ServiceState {
                            name: (*name).to_owned(),
                            state: "running".to_owned(),
                            health: None,
                        })
                        .collect(),
                    ports: Vec::new(),
                    resources: None,
                    blocked_count: 0,
                })),
                Request::Logs(args) => {
                    if self.state != SandboxState::Running {
                        return Err(CallError::Refused(ApiError::new(
                            ErrorCode::WrongState,
                            "not running",
                        )));
                    }
                    if let Some(service) = &args.service {
                        check_service_name(service).map_err(|reason| {
                            CallError::Refused(ApiError::new(ErrorCode::BadRequest, reason))
                        })?;
                    }
                    // A hostile daemon is the point of these tests, so the
                    // bytes go through as they are, lossily, the only way a
                    // text frame could carry them.
                    for chunk in &self.chunks {
                        output(OutputStream::Stdout, &String::from_utf8_lossy(chunk));
                    }
                    Ok(Reply::Logs(LogsReport {
                        code: Some(0),
                        truncated: false,
                    }))
                }
                _ => Err(CallError::Transport("unexpected request".to_owned())),
            }
        }
    }

    fn filled(daemon: &FakeDaemon) -> LogsPanel {
        let mut panel = LogsPanel::default();
        refresh(daemon, &mut panel, "/p");
        panel
    }

    #[test]
    fn a_running_project_lists_its_services_once_each_and_shows_lines() {
        let mut daemon = FakeDaemon::new(SandboxState::Running);
        daemon.chunks = vec![b"web-1 | one\nweb-1 | two\n"];
        let panel = filled(&daemon);
        assert_eq!(panel.services, ["api", "web"]);
        assert_eq!(panel.lines, ["web-1 | one", "web-1 | two"]);
        assert_eq!(panel.note, None);
    }

    #[test]
    fn a_stopped_sandbox_is_not_read_and_the_panel_says_how_to_start_it() {
        for state in [SandboxState::Stopped, SandboxState::None] {
            let daemon = FakeDaemon::new(state);
            let panel = filled(&daemon);
            assert!(!daemon.asked_for_logs(), "{state:?}");
            let note = panel.note.unwrap().text;
            assert!(note.contains("Press 2"), "{note}");
            assert!(note.contains(" up /p"), "{note}");
            assert!(panel.lines.is_empty());
        }
    }

    #[test]
    fn hostile_output_is_filtered_even_split_across_chunks() {
        let mut daemon = FakeDaemon::new(SandboxState::Running);
        daemon.chunks = vec![
            b"a\x1b[3",
            b"1mred\x1b",
            b"]0;evil\x07\ttabbed\x07\r",
            b"\nnext\x1b[2J\n",
        ];
        let panel = filled(&daemon);
        let shown = panel.lines.join("\n");
        assert!(shown.chars().all(|c| !c.is_control() || c == '\n'), "{shown:?}");
        assert!(panel.lines[0].starts_with("ared    tabbed"), "{shown:?}");
        assert_eq!(panel.lines[1], "next");
    }

    #[test]
    fn a_hostile_service_name_cannot_be_chosen() {
        let mut daemon = FakeDaemon::new(SandboxState::Running);
        daemon.services = vec!["-f", "a;b", "web", "x\\u{1b}"];
        let panel = filled(&daemon);
        assert_eq!(panel.services, ["web"]);
    }

    #[test]
    fn a_choice_is_sent_to_the_daemon_and_forgotten_when_it_is_gone() {
        let daemon = FakeDaemon::new(SandboxState::Running);
        let mut panel = LogsPanel {
            chosen: Some("api".to_owned()),
            ..LogsPanel::default()
        };
        refresh(&daemon, &mut panel, "/p");
        assert!(daemon.requests.borrow().iter().any(|r| matches!(
            r,
            Request::Logs(LogsArgs { service: Some(s), follow: false, .. }) if s == "api"
        )));

        panel.chosen = Some("gone".to_owned());
        refresh(&daemon, &mut panel, "/p");
        assert_eq!(panel.chosen, None);
    }

    #[test]
    fn the_panel_never_asks_to_follow() {
        let daemon = FakeDaemon::new(SandboxState::Running);
        filled(&daemon);
        assert!(daemon
            .requests
            .borrow()
            .iter()
            .all(|r| !matches!(r, Request::Logs(LogsArgs { follow: true, .. }))));
    }

    #[test]
    fn enormous_output_does_not_become_an_enormous_panel() {
        let mut lines = Lines::default();
        for _ in 0..(KEPT_LINES + 50) {
            lines.add("line\n");
        }
        lines.add(&"x".repeat(KEPT_LINE_CHARS * 5));
        lines.add(&"x".repeat(KEPT_LINE_CHARS * 5));
        lines.add("\n");
        let kept = lines.finish();
        assert_eq!(kept.len(), KEPT_LINES);
        assert_eq!(kept.last().unwrap().chars().count(), KEPT_LINE_CHARS);
        assert!(kept.last().unwrap().ends_with('…'));
    }

    #[test]
    fn a_daemon_that_cannot_answer_is_said_plainly() {
        struct Silent;
        impl DaemonClient for Silent {
            fn call(
                &self,
                _: Request,
                _: &mut dyn FnMut(&str, Option<&str>),
            ) -> Result<Reply, CallError> {
                Err(CallError::TimedOut)
            }
            fn stream(
                &self,
                _: Request,
                _: &mut dyn FnMut(OutputStream, &str) -> bool,
                limit: Option<Duration>,
            ) -> Result<Reply, CallError> {
                assert!(limit.is_some(), "the panel must always set a time limit");
                Err(CallError::TimedOut)
            }
        }
        let mut panel = LogsPanel::default();
        refresh(&Silent, &mut panel, "/p");
        assert!(panel.note.unwrap().text.contains("did not answer in time"));
    }
}
