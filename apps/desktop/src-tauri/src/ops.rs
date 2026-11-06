//! What the window and the menu bar icon can ask for, without any of Tauri.
//!
//! Each function makes one or more daemon requests and turns the answer into a
//! small view for the web page. The page puts every string it gets from here
//! into the document as text, never as markup. The wording follows the
//! terminal interface, so the two clients say the same thing.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use proto::text::{self, TerminalFilter};
use proto::{
    BlockedArgs, BlockedRecording, BlockedReport, DiffReport, DownArgs, LogTrust, LogsArgs,
    OutputStream, PlanArgs, PlanReport, ProjectArgs, Reply, Request, SandboxState, Source,
    StatusReport, UpArgs,
};
use serde::Serialize;

use crate::client::{CallError, Client, Stopper};

const STATUS_LIMIT: Duration = Duration::from_secs(15);
const PLAN_LIMIT: Duration = Duration::from_secs(60);
const BLOCKED_LIMIT: Duration = Duration::from_secs(15);
const LOGS_LIMIT: Duration = Duration::from_secs(20);
/// A comparison bundles the project inside the sandbox, then reviews it.
const DIFF_LIMIT: Duration = Duration::from_secs(180);
/// How many recent lines to ask for from each service.
pub const LOG_TAIL: u32 = 200;
const MAX_PROJECTS: usize = 200;

pub const HOSTNAMES_ONLY: &str = "This shows blocked hostnames only. Raw IP, metadata and UDP attempts are blocked but not recorded, so an empty list doesn't mean nothing was tried.";
pub const FROM_THE_SANDBOX: &str = "These came from inside the sandbox, so they can be wrong.";
pub const DESTROYED: &str = "Destroyed. Rotate every secret it was given.";
pub const STOPPED: &str = "Stopped. Its data is kept for the next start.";
pub const NO_SANDBOX: &str = "This project has no sandbox, so nothing was changed.";

fn wrong_reply() -> CallError {
    CallError::Transport("The daemon answered something else.".to_owned())
}

/// Where the state folder is, from the same variables the daemon reads.
pub fn locate_state(env: &dyn Fn(&str) -> Option<OsString>) -> Result<hoststate::StatePaths, String> {
    hoststate::StatePaths::locate(env)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RowState {
    Running,
    Stopped,
    None,
    Unknown,
    /// The daemon did not answer for this one.
    NoAnswer,
}

impl RowState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Stopped => "Stopped",
            Self::None => "No sandbox",
            Self::Unknown => "Unknown",
            Self::NoAnswer => "No answer",
        }
    }
}

impl From<SandboxState> for RowState {
    fn from(state: SandboxState) -> Self {
        match state {
            SandboxState::Running => Self::Running,
            SandboxState::Stopped => Self::Stopped,
            SandboxState::None => Self::None,
            SandboxState::Unknown => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectRow {
    /// The folder's own name, for the list.
    pub name: String,
    /// The canonical path the daemon recorded. It is what every request names.
    pub path: String,
    pub state: RowState,
    pub label: &'static str,
}

/// The projects this machine knows about, with each one's state. The list is
/// the state folder's record, read the same way the terminal interface does;
/// the states come from the daemon.
pub fn list_projects(client: &Client, paths: &hoststate::StatePaths, ask_states: bool) -> Vec<ProjectRow> {
    hoststate::known_projects(paths)
        .into_iter()
        .take(MAX_PROJECTS)
        .map(|(id, record)| {
            let name = Path::new(&record.path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| id.as_str().to_owned());
            let state = if ask_states {
                match status(client, &record.path) {
                    Ok(report) => report.sandbox.into(),
                    Err(_) => RowState::NoAnswer,
                }
            } else {
                RowState::NoAnswer
            };
            ProjectRow {
                name: text::escape(&name),
                path: text::escape(&record.path),
                state,
                label: state.label(),
            }
        })
        .collect()
}

pub fn status(client: &Client, path: &str) -> Result<StatusReport, CallError> {
    match client.call(
        Request::Status(ProjectArgs {
            project: path.to_owned(),
        }),
        Some(STATUS_LIMIT),
    )? {
        Reply::Status(report) => Ok(report),
        _ => Err(wrong_reply()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServiceView {
    pub name: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResourceView {
    pub memory: String,
    pub cpus: String,
    pub disk: String,
    pub build_memory: String,
    pub clamped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusView {
    pub path: String,
    pub state: RowState,
    pub label: &'static str,
    pub services: Vec<ServiceView>,
    /// Always on 127.0.0.1.
    pub ports: Vec<String>,
    pub resources: Option<ResourceView>,
    /// `None` when the daemon did not say, which is not zero.
    pub blocked_total: Option<u64>,
    pub blocked_domains: Option<u64>,
    pub blocked_cut_off: bool,
    /// Which secret goes to which service, and every host it could be sent
    /// to. Empty when no secret is assigned. Escaped.
    pub exposure: Vec<String>,
    pub notes: Vec<String>,
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [("TB", 1 << 40), ("GB", 1 << 30), ("MB", 1 << 20), ("KB", 1 << 10)];
    for (unit, size) in UNITS {
        if bytes >= size {
            let value = bytes as f64 / size as f64;
            return if value.fract() == 0.0 {
                format!("{value:.0} {unit}")
            } else {
                format!("{value:.1} {unit}")
            };
        }
    }
    format!("{bytes} B")
}

fn resources(plan: &proto::ResourcePlan) -> ResourceView {
    ResourceView {
        memory: human_bytes(plan.memory_bytes),
        cpus: format!("{}", f64::from(plan.millicpus) / 1000.0),
        disk: human_bytes(plan.disk_bytes),
        build_memory: human_bytes(plan.build_memory_bytes),
        clamped: plan.clamped,
    }
}

pub fn status_view(report: &StatusReport) -> StatusView {
    let state: RowState = report.sandbox.into();
    let mut notes = Vec::new();
    if !report.services.is_empty() {
        notes.push(FROM_THE_SANDBOX.to_owned());
    }
    let (blocked_total, blocked_domains) = match &report.blocked {
        Some(summary) => {
            if let Some(problem) = &summary.problem {
                notes.push(format!("The blocked-traffic log could not be read: {}", text::escape(problem)));
            }
            if summary.recording == BlockedRecording::NotRecording {
                notes.push(not_recording_note());
            }
            (Some(summary.total), Some(summary.domains))
        }
        None => (None, None),
    };
    StatusView {
        path: text::escape(&report.project_path),
        state,
        label: state.label(),
        services: report
            .services
            .iter()
            .take(100)
            .map(|s| ServiceView {
                name: text::escape(&s.name),
                state: match &s.health {
                    Some(health) => format!("{} ({})", text::escape(&s.state), text::escape(health)),
                    None => text::escape(&s.state),
                },
            })
            .collect(),
        ports: report.ports.iter().map(|p| format!("127.0.0.1:{p}")).collect(),
        resources: report.resources.as_ref().map(resources),
        blocked_total,
        blocked_domains,
        blocked_cut_off: report.blocked.as_ref().is_some_and(|b| b.cut_off),
        exposure: report
            .exposure
            .as_ref()
            .map(|e| {
                proto::exposure_lines(&e.secrets, e.reachable_hosts.as_deref(), false)
                    .iter()
                    .map(|line| text::escape(line))
                    .collect()
            })
            .unwrap_or_default(),
        notes,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PortView {
    pub service: String,
    pub host_port: u16,
    pub container_port: u16,
    pub note: Option<String>,
}

/// Everything a person sees before agreeing to start a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanView {
    pub path: String,
    pub digest: String,
    /// The same lines the terminal interface shows in its start box.
    pub summary: Vec<String>,
    pub services: Vec<String>,
    pub commit: Option<String>,
    pub ports: Vec<PortView>,
    pub build_hosts: Vec<String>,
    pub run_hosts: Vec<String>,
    pub secrets: Vec<String>,
    pub resources: ResourceView,
    pub findings: Vec<String>,
    pub warnings: Vec<String>,
    /// For a project with only a Dockerfile, the Compose file the daemon wrote
    /// for it, one escaped line per entry. Empty for a Compose project. It is
    /// part of what the plan's digest covers, so it is shown before agreeing.
    pub generated_compose: Vec<String>,
    /// Which secret goes to which service, and every host it could be sent to.
    pub exposure: Vec<String>,
    /// Reasons it cannot start. When there are any, no start is offered.
    pub blockers: Vec<String>,
}

/// What the terminal interface's start box says, line for line.
pub fn dialog_lines(plan: &PlanReport, ports: &[u16]) -> Vec<String> {
    let mut lines = vec![format!(
        "{} services from commit {}",
        plan.services.len(),
        plan.snapshot
            .revision
            .as_deref()
            .map_or("the working tree", |r| r.get(..12).unwrap_or(r))
    )];
    if ports.is_empty() {
        lines.push("No ports would be forwarded.".to_owned());
    } else {
        lines.push(format!(
            "Forward to 127.0.0.1:  {}",
            ports.iter().map(u16::to_string).collect::<Vec<_>>().join("  ")
        ));
        lines.push("Anything on this machine could then reach them.".to_owned());
    }
    lines.push(format!(
        "{} hosts allowed for the build, {} while it runs.",
        plan.network.build_hosts.len(),
        plan.network.run_hosts.len()
    ));
    if plan.network.merged_for_whole_life {
        lines.push("Build hosts stay reachable after the build.".to_owned());
    }
    lines.into_iter().map(|l| text::escape(&l)).collect()
}

pub fn plan_view(plan: &PlanReport) -> PlanView {
    let ports: Vec<u16> = plan.ports.iter().map(|p| p.host_port).collect();
    let route = |r: proto::SecretRoute| match r {
        proto::SecretRoute::Proxy => "held by the proxy",
        proto::SecretRoute::Injected => "put in the environment",
        proto::SecretRoute::InjectedFile => "written to a file",
        proto::SecretRoute::BuildSecret => "given to the build",
    };
    PlanView {
        path: text::escape(&plan.project_path),
        digest: plan.digest.clone(),
        exposure: proto::exposure_lines(
            &proto::held_from_plan(&plan.secrets),
            Some(&proto::reachable_while_running(&plan.network)),
            false,
        )
        .iter()
        .map(|line| text::escape(line))
        .collect(),
        generated_compose: plan
            .generated_compose
            .as_deref()
            .map(|t| t.lines().map(text::escape).collect())
            .unwrap_or_default(),
        summary: dialog_lines(plan, &ports),
        services: plan.services.iter().map(|s| text::escape(s)).collect(),
        commit: plan.snapshot.revision.as_deref().map(text::escape),
        ports: plan
            .ports
            .iter()
            .map(|p| PortView {
                service: text::escape(&p.service),
                host_port: p.host_port,
                container_port: p.container_port,
                note: match (p.was_all_interfaces, p.dynamic) {
                    (_, true) => Some("A variable can change this number at start.".to_owned()),
                    (true, false) => Some("Asked for every interface; forwarded on 127.0.0.1 only.".to_owned()),
                    (false, false) => None,
                },
            })
            .collect(),
        build_hosts: plan.network.build_hosts.iter().map(|h| text::escape(h)).collect(),
        run_hosts: plan.network.run_hosts.iter().map(|h| text::escape(h)).collect(),
        secrets: plan
            .secrets
            .iter()
            .map(|s| {
                format!(
                    "{}: {} for {}",
                    text::escape(&s.name),
                    route(s.route),
                    if s.services.is_empty() {
                        "no service".to_owned()
                    } else {
                        text::escape(&s.services.join(", "))
                    }
                )
            })
            .collect(),
        resources: resources(&plan.resources),
        findings: plan
            .findings
            .iter()
            .take(50)
            .map(|f| {
                format!(
                    "{}{}: {}",
                    text::escape(&f.severity),
                    f.service
                        .as_deref()
                        .map(|s| format!(" ({})", text::escape(s)))
                        .unwrap_or_default(),
                    text::escape(&f.message)
                )
            })
            .collect(),
        warnings: plan.snapshot.warnings.iter().take(50).map(|w| text::escape(w)).collect(),
        blockers: plan.blockers.iter().map(|b| text::escape(b)).collect(),
    }
}

pub fn plan(client: &Client, path: &str) -> Result<PlanReport, CallError> {
    match client.call(
        Request::Plan(PlanArgs {
            project: path.to_owned(),
            source: Source::default(),
        }),
        Some(PLAN_LIMIT),
    )? {
        Reply::Plan(plan) => Ok(*plan),
        _ => Err(wrong_reply()),
    }
}

/// The plans this app has shown, by project path. A start must name one of
/// them, so the page cannot start something it was never shown, even before
/// the daemon makes its own check against the digest.
#[derive(Default)]
pub struct ShownPlans {
    plans: Mutex<HashMap<String, (String, Vec<u16>)>>,
}

impl ShownPlans {
    pub fn remember(&self, path: &str, plan: &PlanReport) {
        let ports = plan.ports.iter().map(|p| p.host_port).collect();
        self.plans
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path.to_owned(), (plan.digest.clone(), ports));
    }

    /// Takes the plan out, so one approval starts one thing once.
    pub fn take_matching(&self, path: &str, digest: &str, ports: &[u16]) -> Result<(), String> {
        let mut plans = self.plans.lock().unwrap_or_else(|e| e.into_inner());
        let Some((shown, allowed)) = plans.get(path) else {
            return Err("No plan was shown for this project. Press Start again.".to_owned());
        };
        if shown != digest {
            return Err("That is not the plan that was shown. Press Start again.".to_owned());
        }
        if let Some(port) = ports.iter().find(|p| !allowed.contains(p)) {
            return Err(format!("Port {port} was not in the plan that was shown."));
        }
        plans.remove(path);
        Ok(())
    }
}

pub fn up(
    client: &Client,
    path: &str,
    digest: &str,
    ports: Vec<u16>,
    progress: &mut dyn FnMut(&str, Option<&str>),
) -> Result<String, CallError> {
    match client.exchange(
        Request::Up(UpArgs {
            project: path.to_owned(),
            source: Source::default(),
            plan_digest: digest.to_owned(),
            approve_ports: ports,
        }),
        progress,
        &mut |_, _| true,
        None,
        None,
    )? {
        Reply::Up(report) => {
            let mut message = format!("Started in {}s.", report.seconds);
            if !report.ports.is_empty() {
                message.push_str(&format!(
                    " Forwarded to {}.",
                    report
                        .ports
                        .iter()
                        .map(|p| format!("127.0.0.1:{p}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            Ok(message)
        }
        _ => Err(wrong_reply()),
    }
}

pub fn approve_host(client: &Client, path: &str, host: &str) -> Result<String, CallError> {
    let reply = client.call(
        Request::ApproveHost(proto::ApproveHostArgs {
            project: path.to_owned(),
            host: host.to_owned(),
        }),
        None,
    )?;
    let Reply::ApproveHost(report) = reply else {
        return Err(wrong_reply());
    };
    let host = text::escape(&report.host);
    Ok(if report.already_allowed {
        format!("{host} was already allowed. Nothing changed.")
    } else if report.applied {
        format!("{host} is allowed now. Its data was kept, and its services restarted in {}s.", report.seconds)
    } else {
        format!("{host} is approved, and applies when this project starts.")
    })
}

pub fn remove(client: &Client, path: &str) -> Result<String, CallError> {
    let reply = client.call(
        Request::Remove(ProjectArgs {
            project: path.to_owned(),
        }),
        None,
    )?;
    let Reply::Remove(report) = reply else {
        return Err(wrong_reply());
    };
    let mut text = if report.removed_copy {
        "Removed, with its imported copy. The remote itself was not touched.".to_owned()
    } else {
        "Removed from this list. Its folder was not touched.".to_owned()
    };
    if report.sandbox_destroyed {
        text.push_str(" Its sandbox was destroyed: rotate every secret it was given.");
    }
    text.push_str(" Any secrets you stored for it stay in your keychain.");
    Ok(text)
}

pub fn down(client: &Client, path: &str, destroy: bool) -> Result<String, CallError> {
    match client.call(
        Request::Down(DownArgs {
            project: path.to_owned(),
            destroy,
        }),
        None,
    )? {
        Reply::Down(report) if report.destroyed => Ok(DESTROYED.to_owned()),
        Reply::Down(report) if matches!(report.sandbox, SandboxState::None) => Ok(NO_SANDBOX.to_owned()),
        Reply::Down(_) => Ok(STOPPED.to_owned()),
        _ => Err(wrong_reply()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Weight {
    Plain,
    Warn,
    Alert,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Note {
    pub weight: Weight,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockedRow {
    pub time: String,
    pub count: u64,
    pub domain: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockedView {
    pub recording: bool,
    pub total: u64,
    pub rows: Vec<BlockedRow>,
    pub notes: Vec<Note>,
}

fn not_recording_note() -> String {
    format!(
        "Nothing is being recorded for this sandbox, because it was not created at a log level that writes these. To record, it must be recreated with Destroy, then Start, which deletes its data. ({bin} down --destroy, then {bin} up.)",
        bin = brand::CLI_BIN
    )
}

pub fn blocked_view(report: &BlockedReport) -> BlockedView {
    let mut notes = Vec::new();
    let note = |weight, text: String| Note { weight, text };
    match report.recording {
        BlockedRecording::NotRecording => notes.push(note(Weight::Alert, not_recording_note())),
        BlockedRecording::NoSandbox => notes.push(note(
            Weight::Warn,
            "This project has no sandbox, so nothing is being recorded. One started from here records from then on.".to_owned(),
        )),
        BlockedRecording::Recording => {}
    }
    notes.push(note(Weight::Warn, HOSTNAMES_ONLY.to_owned()));
    match &report.trust {
        LogTrust::NotRead => {}
        LogTrust::HostOnly => notes.push(note(
            Weight::Plain,
            "The engine writes this record on the host. As measured, the guest cannot write to its folder, but the guest does choose what it looks up.".to_owned(),
        )),
        LogTrust::Doubtful(reason) => notes.push(note(
            Weight::Alert,
            format!("This record is not trustworthy: {}.", text::escape(reason)),
        )),
    }
    if report.cut_off {
        notes.push(note(
            Weight::Warn,
            "Older events are cut off. The engine keeps about 50 MB of this log, and a busy or hostile sandbox can push events out of it within seconds.".to_owned(),
        ));
    }
    if report.omitted_domains > 0 {
        notes.push(note(
            Weight::Warn,
            format!("{} older names are not listed, because there were too many.", report.omitted_domains),
        ));
    }
    if report.rejected_lines > 0 {
        notes.push(note(
            Weight::Warn,
            format!(
                "{} log lines looked like blocked events but failed the check, so they are not listed.",
                report.rejected_lines
            ),
        ));
    }
    let recording = report.recording == BlockedRecording::Recording;
    if recording && report.events.is_empty() {
        notes.push(note(Weight::Plain, "No blocked hostnames are recorded so far.".to_owned()));
    }
    BlockedView {
        recording,
        total: report.total,
        // Newest first in the window, where the top is what is read first.
        rows: if recording {
            report
                .events
                .iter()
                .rev()
                .take(2000)
                .map(|e| BlockedRow {
                    time: text::escape(&short_time(&e.time)),
                    count: e.count,
                    domain: text::escape(&e.domain),
                })
                .collect()
        } else {
            Vec::new()
        },
        notes,
    }
}

fn short_time(time: &str) -> String {
    let date = time.get(..10).unwrap_or(time);
    let clock = time.get(11..19).unwrap_or("");
    format!("{date} {clock}").trim_end().to_owned()
}

pub fn blocked(client: &Client, path: &str) -> Result<BlockedReport, CallError> {
    match client.call(
        Request::Blocked(BlockedArgs {
            project: path.to_owned(),
            after: None,
        }),
        Some(BLOCKED_LIMIT),
    )? {
        Reply::Blocked(report) => Ok(report),
        _ => Err(wrong_reply()),
    }
}

/// What the Git review page shows. The wording is the terminal's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffView {
    /// The first 12 characters of the commit made of the snapshot.
    pub baseline: String,
    pub flags: Vec<DiffFlagRow>,
    pub files: Vec<DiffFileRow>,
    pub notes: Vec<Note>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffFlagRow {
    /// The path, with a line number when there is one.
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffFileRow {
    pub change: &'static str,
    pub path: String,
}

pub const DIFF_HEURISTIC: &str = "Flags are a heuristic aid. Read the changes yourself before anything from this sandbox reaches your machine.";
pub const NOTHING_COPIED: &str = "Nothing was copied out of the sandbox. Export is not built yet.";

pub fn diff_view(report: &DiffReport) -> DiffView {
    let mut notes = Vec::new();
    let note = |weight, text: &str| Note {
        weight,
        text: text.to_owned(),
    };
    if !report.complete {
        notes.push(note(
            Weight::Alert,
            "The review stopped at a limit, so there may be more to flag than this.",
        ));
    }
    if report.flags.is_empty() {
        notes.push(note(Weight::Warn, &format!("Nothing was flagged. {DIFF_HEURISTIC}")));
    } else {
        let count = report.flags.len();
        notes.push(note(
            Weight::Alert,
            &format!(
                "{count} {} to look at. {DIFF_HEURISTIC}",
                if count == 1 { "flag" } else { "flags" }
            ),
        ));
    }
    if report.files.is_empty() {
        notes.push(note(Weight::Plain, "No files changed."));
    }
    notes.push(note(Weight::Warn, NOTHING_COPIED));
    DiffView {
        baseline: text::escape(report.baseline.get(..12).unwrap_or(&report.baseline)),
        flags: report
            .flags
            .iter()
            .take(2000)
            .map(|flag| {
                let path = text::escape(flag.path.as_deref().unwrap_or("(the whole change)"));
                let mut message = text::escape(&flag.message);
                if flag.earlier_commit {
                    message.push_str(" (in an earlier commit inside the sandbox)");
                }
                DiffFlagRow {
                    path: match flag.line {
                        Some(line) => format!("{path}:{line}"),
                        None => path,
                    },
                    message,
                }
            })
            .collect(),
        files: report
            .files
            .iter()
            .take(5000)
            .map(|file| DiffFileRow {
                change: file.change.word(),
                path: text::escape(&file.path),
            })
            .collect(),
        notes,
    }
}

pub fn diff(client: &Client, path: &str) -> Result<DiffReport, CallError> {
    match client.call(
        Request::Diff(ProjectArgs {
            project: path.to_owned(),
        }),
        Some(DIFF_LIMIT),
    )? {
        Reply::Diff(report) => Ok(report),
        _ => Err(wrong_reply()),
    }
}

/// The names a log can be asked for by. A name from inside the sandbox that
/// fails the check would only be refused.
pub fn log_services(report: &StatusReport) -> Vec<String> {
    let mut names: Vec<String> = report
        .services
        .iter()
        .map(|s| s.name.clone())
        .filter(|n| proto::check_service_name(n).is_ok())
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// How a log read ended, in words.
pub fn logs_end(result: &Result<Reply, CallError>) -> Option<String> {
    match result {
        Ok(Reply::Logs(report)) => match (report.code, report.truncated) {
            (_, true) => Some("The output reached the daemon's limit, so the rest was not sent.".to_owned()),
            (Some(0) | None, false) => None,
            (Some(code), false) => Some(format!("The log command inside the sandbox exited with {code}.")),
        },
        Ok(_) => Some("The daemon answered something else.".to_owned()),
        Err(CallError::Stopped) => None,
        Err(error) => Some(error.message()),
    }
}

/// Reads a project's logs. Text is filtered again here, as everything from the
/// daemon is, and handed over in pieces that need not be whole lines.
/// Following has no time limit; it ends when `stopper` is used.
pub fn logs(
    client: &Client,
    path: &str,
    service: Option<String>,
    follow: bool,
    stopper: &Stopper,
    sink: &mut dyn FnMut(&str) -> bool,
) -> Result<Reply, CallError> {
    if let Some(name) = &service {
        proto::check_service_name(name).map_err(|e| CallError::Transport(e.to_owned()))?;
    }
    let mut stdout = TerminalFilter::new();
    let mut stderr = TerminalFilter::new();
    let result = client.exchange(
        Request::Logs(LogsArgs {
            project: path.to_owned(),
            service,
            tail: LOG_TAIL,
            follow,
        }),
        &mut |_, _| {},
        &mut |stream, text| {
            let filter = match stream {
                OutputStream::Stdout => &mut stdout,
                OutputStream::Stderr => &mut stderr,
            };
            let clean = filter.push(text.as_bytes());
            clean.is_empty() || sink(&clean)
        },
        (!follow).then_some(LOGS_LIMIT),
        Some(stopper),
    );
    for rest in [stdout.finish(), stderr.finish()] {
        if !rest.is_empty() {
            sink(&rest);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diff_report(flags: Vec<proto::DiffFlag>) -> DiffReport {
        DiffReport {
            project_id: "p".to_owned(),
            baseline: "1f2a12002068e9415f0b23be1962202c00fb97dd".to_owned(),
            flags,
            files: vec![proto::DiffFile {
                path: ".claude/settings.json".to_owned(),
                change: proto::FileChange::Added,
            }],
            commits: 1,
            complete: true,
        }
    }

    #[test]
    fn a_review_lists_flags_and_files_and_says_nothing_left_the_sandbox() {
        let view = diff_view(&diff_report(vec![proto::DiffFlag {
            kind: "agent_hook".to_owned(),
            path: Some(".claude/settings.json".to_owned()),
            line: None,
            message: "adds a hook for an AI coding tool, which would run on your machine".to_owned(),
            earlier_commit: false,
        }]));
        assert_eq!(view.baseline, "1f2a12002068");
        assert_eq!(view.flags[0].path, ".claude/settings.json");
        assert_eq!(view.files[0].change, "added");
        assert!(view.notes.iter().any(|n| n.weight == Weight::Alert && n.text.starts_with("1 flag ")));
        assert!(view.notes.iter().any(|n| n.text == NOTHING_COPIED));
    }

    #[test]
    fn a_clean_review_still_says_flags_are_a_heuristic_and_text_is_escaped() {
        let mut report = diff_report(Vec::new());
        report.files[0].path = "a\u{1b}[2Jb\u{202e}".to_owned();
        let view = diff_view(&report);
        assert!(view.flags.is_empty());
        assert!(view.notes.iter().any(|n| n.text.contains(DIFF_HEURISTIC)));
        assert!(!view.files[0].path.contains('\u{1b}') && !view.files[0].path.contains('\u{202e}'));
    }

    #[test]
    fn sizes_read_the_way_a_person_writes_them() {
        assert_eq!(human_bytes(4 << 30), "4 GB");
        assert_eq!(human_bytes(1536 << 20), "1.5 GB");
        assert_eq!(human_bytes(12), "12 B");
    }

    #[test]
    fn a_start_must_name_the_plan_that_was_shown_and_only_once() {
        let shown = ShownPlans::default();
        let plan = sample_plan();
        assert!(shown.take_matching("/p", &plan.digest, &[]).is_err());
        shown.remember("/p", &plan);
        assert!(shown.take_matching("/p", "other", &[3000]).is_err());
        assert!(shown.take_matching("/p", &plan.digest, &[5432]).is_err());
        assert_eq!(shown.take_matching("/p", &plan.digest, &[3000]), Ok(()));
        assert!(shown.take_matching("/p", &plan.digest, &[3000]).is_err());
    }

    #[test]
    fn the_start_summary_matches_the_terminal_box() {
        let plan = sample_plan();
        let lines = dialog_lines(&plan, &[3000]);
        assert_eq!(lines[0], "1 services from commit 0123456789ab");
        assert_eq!(lines[1], "Forward to 127.0.0.1:  3000");
        assert!(lines.iter().any(|l| l == "1 hosts allowed for the build, 0 while it runs."));
    }

    #[test]
    fn text_from_the_sandbox_loses_its_control_characters() {
        let mut plan = sample_plan();
        plan.services = vec!["web\u{1b}[31m\u{202e}".to_owned()];
        let view = plan_view(&plan);
        assert!(!view.services[0].contains('\u{1b}'));
        assert!(!view.services[0].contains('\u{202e}'));
    }

    #[test]
    fn an_empty_blocked_list_says_what_it_does_not_cover() {
        let view = blocked_view(&BlockedReport {
            recording: BlockedRecording::Recording,
            log_level: None,
            events: vec![],
            total: 0,
            cut_off: false,
            omitted_domains: 0,
            rejected_lines: 0,
            trust: LogTrust::NotRead,
            next: None,
        });
        assert!(view.notes.iter().any(|n| n.text == HOSTNAMES_ONLY));
        assert!(view.notes.iter().any(|n| n.text.contains("No blocked hostnames")));
    }

    #[test]
    fn a_generated_compose_file_is_shown_line_by_line_and_escaped() {
        let mut plan = sample_plan();
        assert!(plan_view(&plan).generated_compose.is_empty());
        plan.generated_compose = Some("services:\n  app:\n    image: \u{1b}[31mred\n".to_owned());
        let view = plan_view(&plan);
        assert_eq!(view.generated_compose.len(), 3);
        assert_eq!(view.generated_compose[0], "services:");
        assert!(view.generated_compose.iter().all(|line| !line.contains('\u{1b}')));
    }

    #[test]
    fn the_plan_says_where_a_secret_could_be_sent_including_build_hosts() {
        let mut plan = sample_plan();
        assert!(plan_view(&plan).exposure.is_empty());
        plan.secrets = vec![proto::SecretPlan {
            name: "DEMO_TOKEN".to_owned(),
            route: proto::SecretRoute::Injected,
            services: vec!["web".to_owned()],
            bound_host: None,
        }];
        let lines = plan_view(&plan).exposure;
        assert_eq!(lines[0], "DEMO_TOKEN is given to web");
        // Build hosts stay open in this plan, so they are listed.
        assert!(lines.iter().any(|l| l.trim() == "registry.npmjs.org"), "{lines:?}");
    }

    fn sample_plan() -> PlanReport {
        PlanReport {
            project_id: "p".to_owned(),
            project_path: "/p".to_owned(),
            digest: "d1".to_owned(),
            kind: "compose".to_owned(),
            compose_files: vec!["compose.yaml".to_owned()],
            generated_compose: None,
            services: vec!["web".to_owned()],
            ports: vec![proto::PortPlan {
                service: "web".to_owned(),
                host_port: 3000,
                container_port: 3000,
                was_all_interfaces: true,
                dynamic: false,
            }],
            resources: proto::ResourcePlan {
                memory_bytes: 4 << 30,
                millicpus: 2000,
                disk_bytes: 30 << 30,
                build_memory_bytes: 4 << 30,
                clamped: false,
            },
            network: proto::NetworkPlan {
                build_hosts: vec!["registry.npmjs.org".to_owned()],
                run_hosts: vec![],
                tls: proto::TlsPlan::ServerNameOnly,
                merged_for_whole_life: true,
            },
            secrets: vec![],
            snapshot: proto::SnapshotPlan {
                revision: Some("0123456789abcdef".to_owned()),
                counts: None,
                excluded: vec![],
                warnings: vec![],
            },
            findings: vec![],
            blockers: vec![],
        }
    }
}


// ---------- Git remotes ----------

/// A fetch or a relayed Git command waits on the network and on the person's
/// SSH agent, which may be asking them to approve.
const GIT_LIMIT: Duration = Duration::from_secs(16 * 60);

/// What the window shows after an import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportView {
    pub project_path: String,
    pub commit: Option<String>,
    pub fetched: bool,
    pub message: String,
}

/// Fetches an SSH remote on this machine, with the person's own key, into the
/// daemon's folder. Starting it is a separate step, with its plan.
/// `git_ref` is a branch, tag or commit; `None` takes the remote's default
/// branch. The daemon checks it before anything is fetched.
pub fn import_remote(
    client: &Client,
    url: &str,
    git_ref: Option<String>,
    agent: Option<String>,
) -> Result<ImportView, CallError> {
    let reply = client.call(
        Request::Fetch(proto::FetchArgs {
            url: url.to_owned(),
            git_ref,
            fetch: true,
            ssh_auth_sock: agent,
        }),
        Some(GIT_LIMIT),
    )?;
    let Reply::Fetch(report) = reply else {
        return Err(wrong_reply());
    };
    let message = if report.fetched {
        format!(
            "Fetched {} at {}. Your key stayed on this machine.",
            text::escape(&report.url),
            report.commit.as_deref().unwrap_or("an unknown commit")
        )
    } else {
        "This remote is already in a sandbox, so it was not fetched again. Use Pull on the Git tab.".to_owned()
    };
    Ok(ImportView {
        project_path: report.project_path,
        commit: report.commit,
        fetched: report.fetched,
        message,
    })
}

/// What the Git tab shows after an operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitView {
    pub ok: bool,
    pub title: String,
    /// What Git printed in the sandbox, filtered by the daemon and again here.
    pub output: Vec<String>,
    pub updates: Vec<String>,
    pub branch: Option<String>,
    pub refused: Option<String>,
}

pub fn git(
    client: &Client,
    project: &str,
    op: proto::GitOp,
    identity: Option<proto::GitIdentity>,
    agent: Option<String>,
) -> Result<GitView, CallError> {
    let name = match &op {
        proto::GitOp::Status => "Status",
        proto::GitOp::Pull => "Pull",
        proto::GitOp::Commit { .. } => "Commit",
        proto::GitOp::Push => "Push",
    };
    let reply = client.call(
        Request::Git(proto::GitArgs {
            project: project.to_owned(),
            action: op,
            identity,
            ssh_auth_sock: agent,
        }),
        Some(GIT_LIMIT),
    )?;
    let Reply::Git(report) = reply else {
        return Err(wrong_reply());
    };
    let mut filter = TerminalFilter::new();
    let mut output = filter.push(report.output.as_bytes());
    output.push_str(&filter.finish());
    let outcome = match (&report.refused, report.ok) {
        (Some(_), _) => "refused",
        (None, true) => "done",
        (None, false) => "failed",
    };
    Ok(GitView {
        ok: report.ok,
        title: format!("{name} {outcome}"),
        output: output.lines().take(400).map(str::to_owned).collect(),
        updates: report
            .updates
            .iter()
            .map(|u| format!("{} {:.10} -> {:.10}", text::escape(&u.name), u.old, u.new))
            .collect(),
        branch: report.branch.map(|b| text::escape(&b)),
        refused: report.refused.map(|r| text::escape(&r)),
    })
}

/// The person's name and email from their global Git settings, so commits in
/// the sandbox are theirs. Only the global file is read.
pub fn host_identity() -> Option<proto::GitIdentity> {
    let git = ["/usr/bin/git", "/usr/local/bin/git", "/opt/homebrew/bin/git"]
        .into_iter()
        .find(|path| Path::new(path).is_file())?;
    let read = |key: &str| -> Option<String> {
        let out = std::process::Command::new(git)
            .args(["config", "--global", "--get", key])
            .env_clear()
            .envs(std::env::var_os("HOME").map(|home| ("HOME", home)))
            .current_dir("/")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        let value = String::from_utf8(out.stdout).ok()?.trim().to_owned();
        (out.status.success() && !value.is_empty()).then_some(value)
    };
    let identity = proto::GitIdentity { name: read("user.name")?, email: read("user.email")? };
    proto::check_identity(&identity).is_ok().then_some(identity)
}

/// This app's SSH agent socket, if it has one. A path, never a key.
pub fn agent_socket() -> Option<String> {
    std::env::var("SSH_AUTH_SOCK").ok().filter(|path| !path.is_empty())
}
