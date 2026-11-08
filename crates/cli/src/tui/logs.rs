//! Reading a project's logs into the Logs panel.
//!
//! The panel follows the log, the way `logs -f` does, but on a thread of its
//! own: the interface takes whatever has arrived between keystrokes, so it
//! never waits on a sandbox. Every other call here has a time limit, for the
//! same reason. Only the daemon can read a sandbox, and it refuses one that is
//! not running, but this does not get that far: it asks for the state first,
//! so that it can say how to start one.

use std::time::Duration;

use proto::text::{self, TerminalFilter};
use proto::{check_service_name, LogsArgs, PortChoicesReport, ProjectArgs, Reply, Request, SandboxState, StatusReport};

use super::screen::cut;
use super::style::Style;
use super::view::{Line, Live, LogsPanel};
use crate::client::{Background, CallError, DaemonClient, Event};

/// How long a status request may take.
const STATUS_LIMIT: Duration = Duration::from_secs(15);
/// How long reading logs may take. The daemon has its own, longer, limit.
const READ_LIMIT: Duration = Duration::from_secs(20);
/// How many recent lines to ask for from each service.
const TAIL_LINES: u32 = 200;
/// The most lines kept, however many a sandbox sends.
const KEPT_LINES: usize = 2000;
const KEPT_LINE_CHARS: usize = 1000;
/// The most pieces of output taken in one turn, so that a sandbox writing as
/// fast as it can still leaves the interface time to read a key.
const TAKEN_PER_TURN: usize = 64;
const TAB: &str = "    ";

/// The ports a project publishes, and which are open now and from the next start.
pub fn port_choices(client: &dyn DaemonClient, path: &str) -> Result<PortChoicesReport, CallError> {
    match client.stream(
        Request::PortChoices(ProjectArgs {
            project: path.to_owned(),
        }),
        &mut |_, _| true,
        Some(STATUS_LIMIT),
    ) {
        Ok(Reply::PortChoices(report)) => Ok(report),
        Ok(_) => Err(CallError::Transport(
            "the daemon answered something else".to_owned(),
        )),
        Err(error) => Err(error),
    }
}

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
        "Start it with u on Overview, or run: {} up {}",
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
    open: String,
}

impl Lines {
    fn add(&mut self, text: &str, panel: &mut LogsPanel) {
        for piece in text.split_inclusive('\n') {
            match piece.strip_suffix('\n') {
                Some(end) => {
                    self.append(end);
                    let line = std::mem::take(&mut self.open);
                    panel.push_line(cut(&line, KEPT_LINE_CHARS));
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

    fn finish(&mut self, panel: &mut LogsPanel) {
        if !self.open.is_empty() {
            let line = std::mem::take(&mut self.open);
            panel.push_line(cut(&line, KEPT_LINE_CHARS));
        }
    }
}

/// Filtered again here, as everything from the daemon is before it is shown.
/// Each stream has its own filter, because a control sequence can be split
/// across two pieces of output and the halves have to meet.
#[derive(Default)]
struct Filters {
    stdout: TerminalFilter,
    stderr: TerminalFilter,
    lines: Lines,
}

impl Filters {
    fn add(&mut self, stream: proto::OutputStream, text: &str, panel: &mut LogsPanel) {
        let filter = match stream {
            proto::OutputStream::Stdout => &mut self.stdout,
            proto::OutputStream::Stderr => &mut self.stderr,
        };
        self.lines.add(&filter.push(text.as_bytes()), panel);
    }

    fn finish(&mut self, panel: &mut LogsPanel) {
        let rest = self.stdout.finish();
        self.lines.add(&rest, panel);
        let rest = self.stderr.finish();
        self.lines.add(&rest, panel);
        self.lines.finish(panel);
    }
}

/// A log being followed for the panel. Dropping it stops following, and the
/// daemon stops the log command inside the sandbox.
pub struct Follow {
    call: Background,
    filters: Filters,
    over: bool,
}

impl Follow {
    /// Takes whatever has arrived, without waiting, and says whether the panel
    /// changed and so needs drawing again.
    pub fn take(&mut self, panel: &mut LogsPanel) -> bool {
        let mut changed = false;
        for _ in 0..TAKEN_PER_TURN {
            match self.call.try_next() {
                None => break,
                Some(Event::Output(stream, text)) => {
                    // The note that nothing has arrived yet is no longer true.
                    if panel.note.as_ref().is_some_and(|n| n.style == Style::WindowDim) {
                        panel.note = None;
                    }
                    self.filters.add(stream, &text, panel);
                    changed = true;
                }
                Some(Event::Done(result)) => {
                    self.filters.finish(panel);
                    panel.live = Live::Ended;
                    panel.note = Some(ended(result));
                    self.over = true;
                    changed = true;
                    break;
                }
            }
        }
        if changed {
            panel.trim(KEPT_LINES);
        }
        changed
    }

    /// Whether nothing more will arrive.
    pub fn over(&self) -> bool {
        self.over
    }
}

/// Why following stopped. A log that ends by itself usually means the
/// services stopped, which is worth saying in place of silence.
fn ended(result: Result<Reply, CallError>) -> Line {
    match result {
        Ok(Reply::Logs(report)) => match report.code {
            Some(0) | None => Line::styled(
                "The log ended, so nothing new will appear. Press r to read it again.",
                Style::WindowWarn,
            ),
            Some(code) => Line::styled(
                format!("The log command inside the sandbox exited with {code}. Press r to try again."),
                Style::WindowAlert,
            ),
        },
        Ok(_) => Line::styled("The daemon answered something else.", Style::WindowAlert),
        Err(error) => failure(&error),
    }
}

/// Clears the panel and asks for the state. Returns whether the sandbox is
/// running, which is the only case worth asking for logs.
fn prepare(client: &dyn DaemonClient, panel: &mut LogsPanel, path: &str) -> bool {
    panel.lines.clear();
    panel.note = None;
    panel.live = Live::Off;
    panel.jump_to_newest();
    let report = match status(client, path) {
        Ok(report) => report,
        Err(error) => {
            panel.note = Some(failure(&error));
            return false;
        }
    };
    if report.sandbox != SandboxState::Running {
        panel.services.clear();
        panel.chosen = None;
        panel.note = Some(Line::styled(
            not_running_note(report.sandbox, path),
            Style::WindowWarn,
        ));
        return false;
    }
    panel.services = usable_services(&report);
    if let Some(chosen) = &panel.chosen {
        if !panel.services.contains(chosen) {
            panel.chosen = None;
        }
    }
    true
}

fn logs_request(panel: &LogsPanel, path: &str, follow: bool) -> Request {
    Request::Logs(LogsArgs {
        project: path.to_owned(),
        service: panel.chosen.clone(),
        tail: TAIL_LINES,
        follow,
    })
}

/// Fills the panel for one project and keeps following it, when the client can
/// do that without holding up the interface. Otherwise it reads once, as
/// [`refresh`] does. The chosen service survives, and is forgotten only if the
/// project no longer has it.
pub fn follow(client: &dyn DaemonClient, panel: &mut LogsPanel, path: &str) -> Option<Follow> {
    if !prepare(client, panel, path) {
        return None;
    }
    let Some(call) = client.background(logs_request(panel, path, true)) else {
        read_once(client, panel, path);
        return None;
    };
    panel.live = Live::Following;
    panel.note = Some(Line::dim(
        "Nothing has been logged yet. New lines appear here as they are written.",
    ));
    Some(Follow {
        call,
        filters: Filters::default(),
        over: false,
    })
}

/// Fills the panel for one project with one read, with a time limit. The
/// interface reaches this only through [`follow`]; the tests use it to check
/// the read on its own, whatever the client can do.
#[cfg(test)]
fn refresh(client: &dyn DaemonClient, panel: &mut LogsPanel, path: &str) {
    if prepare(client, panel, path) {
        read_once(client, panel, path);
    }
}

fn read_once(client: &dyn DaemonClient, panel: &mut LogsPanel, path: &str) {
    let mut filters = Filters::default();
    let request = logs_request(panel, path, false);
    let result = client.stream(
        request,
        &mut |stream, text| {
            filters.add(stream, text, panel);
            true
        },
        Some(READ_LIMIT),
    );
    filters.finish(panel);
    panel.trim(KEPT_LINES);

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

pub(super) fn failure(error: &CallError) -> Line {
    let message = match error {
        CallError::TimedOut => "The daemon did not answer in time. Press r to try again.".to_owned(),
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
                    blocked: None,
                    exposure: None,
                    usage: None,
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
            assert!(note.contains("u on Overview"), "{note}");
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
    fn a_single_read_never_asks_to_follow() {
        let daemon = FakeDaemon::new(SandboxState::Running);
        filled(&daemon);
        assert!(daemon
            .requests
            .borrow()
            .iter()
            .all(|r| !matches!(r, Request::Logs(LogsArgs { follow: true, .. }))));
    }

    /// A client that can run a call in the background, fed by the test.
    struct Follower {
        inner: FakeDaemon,
        sender: RefCell<Option<std::sync::mpsc::SyncSender<Event>>>,
        stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
        asked: RefCell<Vec<Request>>,
    }

    impl Follower {
        fn new(state: SandboxState) -> Self {
            Self {
                inner: FakeDaemon::new(state),
                sender: RefCell::new(None),
                stopped: std::sync::Arc::default(),
                asked: RefCell::new(Vec::new()),
            }
        }

        fn send(&self, event: Event) {
            self.sender.borrow().as_ref().unwrap().send(event).unwrap();
        }

        fn out(&self, text: &[u8]) {
            self.send(Event::Output(
                OutputStream::Stdout,
                String::from_utf8_lossy(text).into_owned(),
            ));
        }

        fn stopped(&self) -> bool {
            self.stopped.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl DaemonClient for Follower {
        fn call(
            &self,
            request: Request,
            progress: &mut dyn FnMut(&str, Option<&str>),
        ) -> Result<Reply, CallError> {
            self.inner.call(request, progress)
        }

        fn stream(
            &self,
            request: Request,
            output: &mut dyn FnMut(OutputStream, &str) -> bool,
            limit: Option<Duration>,
        ) -> Result<Reply, CallError> {
            assert!(limit.is_some(), "the panel must always set a time limit");
            self.inner.stream(request, output, limit)
        }

        fn background(&self, request: Request) -> Option<Background> {
            self.asked.borrow_mut().push(request);
            let (sender, events) = std::sync::mpsc::sync_channel(64);
            *self.sender.borrow_mut() = Some(sender);
            let stopped = self.stopped.clone();
            Some(Background::new(events, move || {
                stopped.store(true, std::sync::atomic::Ordering::SeqCst);
            }))
        }
    }

    #[test]
    fn following_asks_to_follow_and_adds_lines_as_they_arrive() {
        let daemon = Follower::new(SandboxState::Running);
        let mut panel = LogsPanel::default();
        let mut follow = follow(&daemon, &mut panel, "/p").expect("a running sandbox is followed");
        assert!(matches!(
            daemon.asked.borrow()[0],
            Request::Logs(LogsArgs { follow: true, .. })
        ));
        assert!(!daemon.inner.asked_for_logs(), "it must not also read once");
        assert_eq!(panel.live, Live::Following);
        assert!(panel.note.as_ref().unwrap().text.contains("Nothing has been logged"));
        assert!(!follow.take(&mut panel), "nothing has arrived yet");

        daemon.out(b"one\ntw");
        assert!(follow.take(&mut panel));
        assert_eq!(panel.lines, ["one"]);
        assert_eq!(panel.note, None);
        daemon.out(b"o\nthree\n");
        follow.take(&mut panel);
        assert_eq!(panel.lines, ["one", "two", "three"]);
        assert!(!follow.over());
    }

    #[test]
    fn hostile_output_is_filtered_while_following_too() {
        let daemon = Follower::new(SandboxState::Running);
        let mut panel = LogsPanel::default();
        let mut follow = follow(&daemon, &mut panel, "/p").unwrap();
        daemon.out(b"a\x1b[3");
        daemon.out(b"1mred\x1b]0;evil\x07\x1b[2J\n");
        follow.take(&mut panel);
        assert_eq!(panel.lines, ["ared"]);
    }

    #[test]
    fn the_end_of_a_followed_log_is_said_and_keeps_its_last_line() {
        let daemon = Follower::new(SandboxState::Running);
        let mut panel = LogsPanel::default();
        let mut follow = follow(&daemon, &mut panel, "/p").unwrap();
        daemon.out(b"no newline at the end");
        daemon.send(Event::Done(Ok(Reply::Logs(LogsReport {
            code: Some(0),
            truncated: false,
        }))));
        assert!(follow.take(&mut panel));
        assert!(follow.over());
        assert_eq!(panel.live, Live::Ended);
        assert_eq!(panel.lines, ["no newline at the end"]);
        assert!(panel.note.unwrap().text.contains("Press r"));
    }

    #[test]
    fn a_followed_log_that_fails_says_why() {
        let daemon = Follower::new(SandboxState::Running);
        let mut panel = LogsPanel::default();
        let mut follow = follow(&daemon, &mut panel, "/p").unwrap();
        daemon.send(Event::Done(Err(CallError::Transport("gone\x1b[2J".to_owned()))));
        follow.take(&mut panel);
        let note = panel.note.unwrap();
        assert_eq!(note.style, Style::WindowAlert);
        assert!(!note.text.contains('\x1b'), "{note:?}");
    }

    #[test]
    fn dropping_a_follow_stops_the_call() {
        let daemon = Follower::new(SandboxState::Running);
        let mut panel = LogsPanel::default();
        let follow = follow(&daemon, &mut panel, "/p").unwrap();
        assert!(!daemon.stopped());
        drop(follow);
        assert!(daemon.stopped());
    }

    #[test]
    fn a_stopped_sandbox_is_not_followed() {
        let daemon = Follower::new(SandboxState::Stopped);
        let mut panel = LogsPanel::default();
        assert!(follow(&daemon, &mut panel, "/p").is_none());
        assert!(daemon.asked.borrow().is_empty());
        assert_eq!(panel.live, Live::Off);
    }

    /// Without a way to follow, the panel still fills, once, with a limit.
    #[test]
    fn a_client_that_cannot_follow_is_read_once() {
        let mut daemon = FakeDaemon::new(SandboxState::Running);
        daemon.chunks = vec![b"only\n"];
        let mut panel = LogsPanel::default();
        assert!(follow(&daemon, &mut panel, "/p").is_none());
        assert_eq!(panel.lines, ["only"]);
        assert_eq!(panel.live, Live::Off);
    }

    /// Lines that arrive while someone has scrolled back are kept, and what
    /// they are reading stays where it is.
    #[test]
    fn a_paused_view_stays_put_as_lines_arrive() {
        let daemon = Follower::new(SandboxState::Running);
        let mut panel = LogsPanel::default();
        let mut follow = follow(&daemon, &mut panel, "/p").unwrap();
        daemon.out(b"a\nb\nc\n");
        follow.take(&mut panel);
        panel.scroll_by(1);
        let looking_at = panel.lines.len() - panel.scroll;
        daemon.out(b"d\ne\n");
        follow.take(&mut panel);
        assert_eq!(panel.lines.len() - panel.scroll, looking_at);
        assert_eq!(panel.unseen, 2);
        panel.scroll_by(-100);
        assert!(!panel.paused());
        assert_eq!(panel.unseen, 0);
    }

    #[test]
    fn enormous_output_does_not_become_an_enormous_panel() {
        let mut panel = LogsPanel::default();
        let mut lines = Lines::default();
        for _ in 0..(KEPT_LINES + 50) {
            lines.add("line\n", &mut panel);
        }
        lines.add(&"x".repeat(KEPT_LINE_CHARS * 5), &mut panel);
        lines.add(&"x".repeat(KEPT_LINE_CHARS * 5), &mut panel);
        lines.add("\n", &mut panel);
        lines.finish(&mut panel);
        panel.trim(KEPT_LINES);
        let kept = panel.lines;
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
