//! Reading blocked hostnames into the Traffic panel.
//!
//! The call has a time limit, because the interface cannot draw or read a key
//! while it waits.

use std::time::Duration;

use crate::blocked::{self, Weight};
use crate::client::{CallError, DaemonClient};

use super::logs::failure;
use super::style::Style;
use super::view::{Line, TrafficPanel};

const READ_LIMIT: Duration = Duration::from_secs(15);

fn styled(weight: Weight, text: String) -> Line {
    Line::styled(
        text,
        match weight {
            Weight::Plain => Style::WindowDim,
            Weight::Warn => Style::WindowWarn,
            Weight::Alert => Style::WindowAlert,
        },
    )
}

/// Fills the panel for one project. What it held is dropped first, so a failure
/// cannot leave an old list looking current.
pub fn refresh(client: &dyn DaemonClient, panel: &mut TrafficPanel, path: &str) {
    panel.rows.clear();
    panel.hosts.clear();
    panel.notes.clear();
    panel.allowed.clear();
    panel.scroll = 0;
    panel.allowed = match ask_hosts(client, path) {
        Ok(report) => allowed_lines(&report),
        // A daemon older than this list, or one that did not answer: the
        // blocked list below still stands.
        Err(_) => vec![Line::styled("The allowed hosts could not be read.".to_owned(), Style::WindowDim)],
    };
    match blocked::ask(client, path, None, READ_LIMIT) {
        Ok(report) => {
            panel.notes = blocked::notes(&report)
                .into_iter()
                // The panel always draws this one itself.
                .filter(|(_, text)| text != blocked::HOSTNAMES_ONLY)
                .map(|(weight, text)| styled(weight, text))
                .collect();
            if report.recording == proto::BlockedRecording::Recording {
                if report.events.is_empty() {
                    panel
                        .notes
                        .push(Line::plain("No blocked hostnames are recorded so far."));
                }
                panel.rows = report.events.iter().map(blocked::row).collect();
                panel.hosts = report.events.iter().map(|event| event.domain.clone()).collect();
            }
        }
        Err(error) => panel.notes.push(failure(&error)),
    }
}

fn ask_hosts(client: &dyn DaemonClient, path: &str) -> Result<proto::HostsReport, CallError> {
    match client.stream(
        proto::Request::Hosts(proto::ProjectArgs { project: path.to_owned() }),
        &mut |_, _| true,
        Some(READ_LIMIT),
    ) {
        Ok(proto::Reply::Hosts(report)) => Ok(report),
        Ok(_) => Err(CallError::Transport("the daemon answered something else".to_owned())),
        Err(error) => Err(error),
    }
}

/// What the sandbox may reach, kept short: the plan's hosts are counted, not
/// listed, so the blocked list keeps its room.
fn allowed_lines(report: &proto::HostsReport) -> Vec<Line> {
    let esc = crate::ui::esc;
    let mut out = vec![Line::styled("APPROVED BY YOU".to_owned(), Style::WindowDim)];
    if report.approved.is_empty() {
        out.push(Line::styled("None.".to_owned(), Style::WindowDim));
    }
    for host in &report.approved {
        let state = if host.in_effect { "reachable now" } else { "from the next start" };
        out.push(Line::styled(format!("{}  {state}", esc(&host.host)), Style::Window));
    }
    for host in &report.leaving {
        out.push(Line::styled(format!("{}  blocked again, until it is replaced", esc(host)), Style::WindowWarn));
    }
    let plan = match report.plan_hosts.len() {
        0 => "From the plan: none.".to_owned(),
        1 => "From the plan: 1 build host".to_owned(),
        n => format!("From the plan: {n} build hosts"),
    };
    let plan = if report.plan_hosts_while_running && !report.plan_hosts.is_empty() {
        format!("{plan}, reachable while it runs too.")
    } else if report.plan_hosts.is_empty() {
        plan
    } else {
        format!("{plan}.")
    };
    out.push(Line::styled(plan, Style::WindowDim));
    out.push(Line::styled(
        format!("Block one again with: {} net block NAME", brand::CLI_BIN),
        Style::WindowDim,
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::{
        ApiError, BlockedEvent, BlockedRecording, BlockedReport, ErrorCode, LogTrust, OutputStream,
        Reply, Request,
    };
    use std::cell::Cell;

    /// Answers as the real daemon does. A limit is demanded, and a call made
    /// without one fails the test, because that is how the interface would hang.
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
            if matches!(request, Request::Hosts(_)) {
                assert!(limit.is_some(), "the panel must always set a time limit");
                return Ok(Reply::Hosts(proto::HostsReport {
                    sandbox: proto::SandboxState::Running,
                    plan_hosts: vec!["registry-1.docker.io".into(), "auth.docker.io".into()],
                    plan_hosts_while_running: true,
                    plan_note: None,
                    approved: vec![proto::ApprovedHost {
                        host: "example.com".into(),
                        approved_unix: Some(1),
                        in_effect: true,
                    }],
                    leaving: vec!["old.example.org".into()],
                }));
            }
            assert!(matches!(request, Request::Blocked(_)));
            self.limited.set(limit.is_some());
            (self.answer)()
        }
    }

    fn base(recording: BlockedRecording) -> BlockedReport {
        BlockedReport {
            recording,
            log_level: None,
            events: Vec::new(),
            total: 0,
            cut_off: false,
            omitted_domains: 0,
            rejected_lines: 0,
            unlisted_domains: 0,
            trust: LogTrust::NotRead,
            next: None,
        }
    }

    fn filled(answer: fn() -> Result<Reply, CallError>) -> TrafficPanel {
        let daemon = FakeDaemon {
            answer,
            limited: Cell::new(false),
        };
        let mut panel = TrafficPanel {
            rows: vec!["left over".to_owned()],
            scroll: 9,
            ..TrafficPanel::default()
        };
        refresh(&daemon, &mut panel, "/p");
        assert!(daemon.limited.get(), "the panel must always set a time limit");
        panel
    }

    fn notes_of(panel: &TrafficPanel) -> String {
        panel
            .notes
            .iter()
            .map(|n| n.text.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn rows_arrive_in_order_and_the_view_starts_at_the_newest() {
        let panel = filled(|| {
            let mut r = base(BlockedRecording::Recording);
            r.trust = LogTrust::HostOnly;
            r.events = vec![
                BlockedEvent {
                    time: "2026-11-05T07:50:01.000000Z".to_owned(),
                    domain: "old.example.net".to_owned(),
                    count: 2,
                    allowed: false,
                },
                BlockedEvent {
                    time: "2026-11-05T07:50:02.000000Z".to_owned(),
                    domain: "new.example.net".to_owned(),
                    count: 1,
                    allowed: false,
                },
            ];
            Ok(Reply::Blocked(r))
        });
        assert_eq!(panel.scroll, 0);
        assert_eq!(panel.rows.len(), 2);
        assert!(panel.rows[1].contains("new.example.net"));
        assert!(notes_of(&panel).contains("the guest cannot write"));
    }

    #[test]
    fn a_sandbox_that_records_nothing_says_so_rather_than_showing_an_empty_list() {
        let panel = filled(|| Ok(Reply::Blocked(base(BlockedRecording::NotRecording))));
        let text = notes_of(&panel);
        assert!(text.contains("Nothing is being recorded"), "{text}");
        assert!(text.contains("down --destroy"), "{text}");
        assert!(!text.contains("recorded so far"), "{text}");
        assert!(panel.rows.is_empty());
    }

    #[test]
    fn an_empty_list_while_recording_says_only_that_nothing_is_recorded_so_far() {
        let panel = filled(|| {
            let mut r = base(BlockedRecording::Recording);
            r.trust = LogTrust::HostOnly;
            Ok(Reply::Blocked(r))
        });
        assert!(notes_of(&panel).contains("recorded so far"));
    }

    #[test]
    fn a_doubtful_record_is_called_out() {
        let panel = filled(|| {
            let mut r = base(BlockedRecording::Recording);
            r.trust = LogTrust::Doubtful("reason".to_owned());
            Ok(Reply::Blocked(r))
        });
        assert!(notes_of(&panel).contains("not trustworthy"));
        assert!(panel
            .notes
            .iter()
            .any(|n| n.style == Style::WindowAlert && n.text.contains("not trustworthy")));
    }

    #[test]
    fn a_daemon_that_does_not_answer_clears_the_old_list_and_says_so() {
        let panel = filled(|| Err(CallError::TimedOut));
        assert!(panel.rows.is_empty(), "an old list must not look current");
        assert!(notes_of(&panel).contains("did not answer in time"));
    }

    #[test]
    fn a_refusal_is_shown_on_one_row() {
        let panel = filled(|| {
            Err(CallError::Refused(ApiError::new(
                ErrorCode::Refused,
                "runtime.log is a symbolic link",
            )))
        });
        assert!(notes_of(&panel).contains("symbolic link"));
    }

    #[test]
    fn the_allowed_hosts_come_before_the_blocked_list_and_say_how_to_change_them() {
        let panel = filled(|| Ok(Reply::Blocked(base(BlockedRecording::Recording))));
        let text: Vec<&str> = panel.allowed.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(text[0], "APPROVED BY YOU");
        assert_eq!(text[1], "example.com  reachable now");
        assert_eq!(text[2], "old.example.org  blocked again, until it is replaced");
        // Wrapped by the panel at any width; these fit 60 columns whole.
        assert_eq!(text[3], "From the plan: 2 build hosts, reachable while it runs too.");
        assert!(text[4].contains("net block NAME"));
        assert!(text.iter().all(|t| t.chars().count() <= 60), "{text:?}");
    }
}
