//! The daemon's wire protocol: a narrow, versioned JSON API.
//!
//! Every client speaks this and nothing else, so a graphical client, a terminal
//! interface and the command line all have exactly the same powers. The daemon
//! holds the secrets, the policy and the git credentials; a client only asks.
//!
//! # Shape of a call
//!
//! A client opens the socket, writes one [`Envelope`], then reads [`Frame`]s
//! until it sees [`Frame::Done`]. The connection is then finished. One
//! connection carries one request, so there is no session state to confuse and
//! nothing to replay.
//!
//! # What a client may decide
//!
//! A client never sends policy. It cannot name the hosts a sandbox may reach,
//! its memory limit or where its secrets come from: the daemon works all of
//! that out from the project's own files and the user's stored settings. A
//! client only passes on choices a person made, and the daemon checks each one
//! against the plan it built itself. [`UpArgs::approve_ports`] is the clearest
//! case: ports not in the daemon's own plan are refused, so a client cannot
//! open one by asking.
//!
//! # Text
//!
//! Every string the daemon sends has already been through [`text::escape`].
//! It may still be a lie, because it can come from a compromised sandbox, but
//! it cannot carry terminal control sequences or hidden bidi characters. Log
//! output goes through [`text::TerminalFilter`] instead, which also keeps its
//! tabs and newlines.
//!
//! # Compatibility
//!
//! A new request is an addition rather than a new version. A daemon that does
//! not know it refuses that one request, and every other request keeps working.

pub mod text;
pub mod wire;

use serde::{Deserialize, Serialize};

pub use wire::{read_frame, write_frame, WireError, MAX_FRAME_BYTES};

/// The protocol version. A client and daemon that disagree refuse to talk,
/// rather than guessing at a field's meaning.
pub const PROTOCOL_VERSION: u32 = 1;

/// What a client writes: exactly one of these, then it reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub version: u32,
    pub request: Request,
}

impl Envelope {
    pub fn new(request: Request) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            request,
        }
    }
}

/// The complete set of things a client may ask for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Work out what starting this project would do, without doing any of it.
    Plan(PlanArgs),
    /// Start a project, after the person approved a plan.
    Up(UpArgs),
    /// Stop a project, or remove it and everything in it.
    Down(DownArgs),
    /// Report on a project's sandbox.
    Status(ProjectArgs),
    /// Read what a project's services have logged. Never starts anything.
    Logs(LogsArgs),
    /// Ask how to open a terminal in a running sandbox. Never starts anything.
    Terminal(TerminalArgs),
}

/// The most log lines one request may ask for from each service.
pub const MAX_LOG_TAIL: u32 = 100_000;
const MAX_SERVICE_NAME_BYTES: usize = 128;

/// Checks that a Compose service name is only a name. It goes on a command
/// line inside the sandbox, so it must not be readable as a flag.
pub fn check_service_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("a service name must not be empty");
    }
    if name.len() > MAX_SERVICE_NAME_BYTES {
        return Err("that service name is too long");
    }
    if name.starts_with('-') {
        return Err("a service name must not start with '-'");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err("a service name may only use letters, digits, '.', '_' and '-'");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogsArgs {
    pub project: String,
    /// One service, or `None` for all of them.
    pub service: Option<String>,
    /// How many recent lines to show from each service.
    pub tail: u32,
    /// Keep going until the client disconnects.
    pub follow: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalArgs {
    pub project: String,
    /// The service the terminal is for, which the daemon checks and records.
    pub service: Option<String>,
}

/// Naming a project. The daemon canonicalises the path itself and derives the
/// project's identity from it; a client cannot supply an identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectArgs {
    pub project: String,
}

/// Where a snapshot's files come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    /// A committed revision. Nothing uncommitted reaches the sandbox.
    Commit { revision: String },
    /// The working tree as it is now, including files not committed. The
    /// command line requires an explicit flag for this, because it is the one
    /// way an unreviewed local file reaches the sandbox.
    WorkingTree,
}

impl Default for Source {
    fn default() -> Self {
        Self::Commit {
            revision: "HEAD".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanArgs {
    pub project: String,
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpArgs {
    pub project: String,
    pub source: Source,
    /// The digest of the plan the person approved. The daemon rebuilds the
    /// plan and refuses if it no longer matches, so a project file edited
    /// between the plan and the approval cannot slip through.
    pub plan_digest: String,
    /// Host ports to forward. Each must appear in the daemon's own plan.
    pub approve_ports: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownArgs {
    pub project: String,
    /// Remove the sandbox and everything in it, rather than stopping it.
    pub destroy: bool,
}

/// What the daemon writes back, in order, until [`Frame::Done`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "frame", rename_all = "snake_case", deny_unknown_fields)]
pub enum Frame {
    /// A step finished. Clients may show these as they arrive.
    Progress { step: String, detail: Option<String> },
    /// Some of a command's output, already filtered. It can end anywhere, so
    /// it is not necessarily a whole line.
    Output { stream: OutputStream, text: String },
    /// The last frame on a connection.
    Done(Outcome),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Ok { reply: Reply },
    Error { error: ApiError },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    // Boxed because a plan is far larger than the other replies, and every
    // frame on the wire would otherwise be sized for it.
    Plan(Box<PlanReport>),
    Up(UpReport),
    Down(DownReport),
    Status(StatusReport),
    Logs(LogsReport),
    Terminal(TerminalReport),
}

/// How a log request ended. The output itself came in [`Frame::Output`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogsReport {
    /// What the log command exited with, or `None` if it was stopped first.
    pub code: Option<i32>,
    /// The output reached the daemon's limit, so the rest was not sent.
    pub truncated: bool,
}

/// What a client needs in order to run the engine on the person's own
/// terminal. No secret is here, and a process of the same user could already
/// read all of it from the state folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalReport {
    pub sandbox: String,
    /// The absolute path of the engine's binary.
    pub engine_binary: String,
    /// The absolute path of the engine's state folder.
    pub engine_home: String,
    /// The project's folder inside the sandbox.
    pub workdir: String,
}

/// Why a request did not succeed. The message is already safe to print.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl AsRef<str>) -> Self {
        Self {
            code,
            message: text::escape_short(message.as_ref(), 2000),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The request was malformed, or asked for something outside the plan.
    BadRequest,
    /// The protocol versions do not match.
    Version,
    /// No such project, or no sandbox for it.
    NotFound,
    /// The sandbox is in the wrong state for this.
    WrongState,
    /// A person refused, or a check failed.
    Refused,
    /// The engine is missing or unusable on this machine.
    Engine,
    /// Built, but not usable on this platform or in this situation.
    Unsupported,
    /// The daemon failed in a way that is its own fault.
    Internal,
}

/// What `up` would do. Everything a person needs in order to approve.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanReport {
    pub project_id: String,
    pub project_path: String,
    /// Covers everything below. [`UpArgs::plan_digest`] must equal this.
    pub digest: String,
    pub kind: String,
    /// The Compose files, in the order they are merged, as paths inside the
    /// project. Any override the project carries is in here, so starting it
    /// reads exactly what the project would read on its own.
    pub compose_files: Vec<String>,
    pub services: Vec<String>,
    pub ports: Vec<PortPlan>,
    pub resources: ResourcePlan,
    pub network: NetworkPlan,
    pub secrets: Vec<SecretPlan>,
    pub snapshot: SnapshotPlan,
    /// What the static checks found in the project's files.
    pub findings: Vec<Finding>,
    /// Reasons this plan cannot be started as it is.
    pub blockers: Vec<String>,
}

/// A port the project asks to publish. Every one is forwarded to `127.0.0.1`
/// and keeps its number, so there is no bind address or remapping here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortPlan {
    pub service: String,
    pub host_port: u16,
    pub container_port: u16,
    /// The project published this on every interface. Inside the sandbox that
    /// is harmless; on the host it is still only `127.0.0.1`.
    pub was_all_interfaces: bool,
    /// A variable can change this at start time, so the number here is a guess
    /// from the file's defaults.
    pub dynamic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourcePlan {
    pub memory_bytes: u64,
    pub millicpus: u32,
    pub disk_bytes: u64,
    pub build_memory_bytes: u64,
    /// Requests the user's own maximum lowered.
    pub clamped: bool,
}

/// The outbound rules. Deny by default; an empty `hosts` means nothing leaves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkPlan {
    /// Allowed during the build only.
    pub build_hosts: Vec<String>,
    /// Allowed while the project runs.
    pub run_hosts: Vec<String>,
    pub tls: TlsPlan,
    /// True when the build and run lists had to be merged into one set for the
    /// sandbox's whole life. The engine spike did not establish that a
    /// sandbox's network policy can change between build and run, so until it
    /// does, build hosts stay reachable while the project runs.
    pub merged_for_whole_life: bool,
}

/// How HTTPS to an allowed name is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TlsPlan {
    /// Decided from the TLS server name, without decrypting. Nested containers
    /// and build steps need no extra certificate authority, and no secret can
    /// be swapped in.
    ServerNameOnly,
    /// The gateway terminates TLS, which is what proxy-held secrets need.
    /// Nested containers and build steps do not trust its authority by
    /// default, so their HTTPS fails until it is added.
    Intercept,
}

/// One secret, by name and route. No value ever appears here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretPlan {
    pub name: String,
    pub route: SecretRoute,
    pub services: Vec<String>,
    /// For a proxy secret, the one host its value is added for.
    pub bound_host: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretRoute {
    /// The sandbox holds a placeholder and the host swaps in the real value.
    Proxy,
    /// The real value is put into the environment of named services only.
    /// Every program already reads its environment, so a project works
    /// unchanged, but everything in that container can read it too.
    Injected,
    /// The real value is written where named services can read it, at
    /// `/run/secrets/<name>`. Narrower, but the program must read a file.
    InjectedFile,
    /// The real value is passed to one build step, opt-in.
    BuildSecret,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotPlan {
    /// The revision the files come from, or `None` for the working tree.
    pub revision: Option<String>,
    /// What the snapshot holds, once one has actually been taken. `None`
    /// means it has not been taken yet, which is not the same as empty.
    pub counts: Option<SnapshotCounts>,
    /// Files deliberately left out, with the reason.
    pub excluded: Vec<ExcludedFile>,
    /// Files copied in that look like they hold a secret.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotCounts {
    pub files: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExcludedFile {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub severity: String,
    pub service: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpReport {
    pub project_id: String,
    pub sandbox: SandboxState,
    pub services: Vec<ServiceState>,
    /// Ports actually forwarded, on `127.0.0.1`.
    pub ports: Vec<u16>,
    pub seconds: u64,
    /// Which services were given which secrets. Names only.
    pub secrets: Vec<SecretsDelivered>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsDelivered {
    pub service: String,
    pub names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownReport {
    pub project_id: String,
    pub sandbox: SandboxState,
    pub destroyed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusReport {
    pub project_id: String,
    pub project_path: String,
    pub sandbox: SandboxState,
    pub services: Vec<ServiceState>,
    pub ports: Vec<u16>,
    pub resources: Option<ResourcePlan>,
    /// Blocked outbound requests the host recorded for this project.
    pub blocked_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxState {
    None,
    Stopped,
    Running,
    /// A state the adapter has not seen before.
    Unknown,
}

/// One service inside the sandbox, as the sandbox reported it. A compromised
/// sandbox can lie here, so this is information, not evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceState {
    pub name: String,
    pub state: String,
    pub health: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(request: Request) {
        let envelope = Envelope::new(request.clone());
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &envelope).unwrap();
        let back: Envelope = read_frame(&mut buffer.as_slice()).unwrap().unwrap();
        assert_eq!(back.request, request);
        assert_eq!(back.version, PROTOCOL_VERSION);
    }

    #[test]
    fn every_request_survives_the_wire() {
        round_trip(Request::Plan(PlanArgs {
            project: "/p".to_owned(),
            source: Source::default(),
        }));
        round_trip(Request::Up(UpArgs {
            project: "/p".to_owned(),
            source: Source::WorkingTree,
            plan_digest: "abc".to_owned(),
            approve_ports: vec![3000],
        }));
        round_trip(Request::Down(DownArgs {
            project: "/p".to_owned(),
            destroy: true,
        }));
        round_trip(Request::Status(ProjectArgs {
            project: "/p".to_owned(),
        }));
        round_trip(Request::Logs(LogsArgs {
            project: "/p".to_owned(),
            service: Some("web".to_owned()),
            tail: 200,
            follow: true,
        }));
        round_trip(Request::Terminal(TerminalArgs {
            project: "/p".to_owned(),
            service: None,
        }));
    }

    #[test]
    fn output_survives_the_wire_as_one_frame() {
        let frame = Frame::Output {
            stream: OutputStream::Stderr,
            text: "a\tb\nc".to_owned(),
        };
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &frame).unwrap();
        assert_eq!(buffer.iter().filter(|b| **b == b'\n').count(), 1);
        let back: Frame = read_frame(&mut buffer.as_slice()).unwrap().unwrap();
        assert_eq!(back, frame);
    }

    #[test]
    fn ordinary_service_names_are_accepted() {
        for name in ["web", "api-1", "my_service", "a.b", "A9", "worker.2_x-y"] {
            assert_eq!(check_service_name(name), Ok(()), "{name}");
        }
    }

    #[test]
    fn a_service_name_that_could_be_anything_else_is_refused() {
        let long = "a".repeat(129);
        for name in [
            "",
            "-f",
            "--follow",
            "-",
            "a;b",
            "a b",
            "a/b",
            "$(x)",
            "`x`",
            "a|b",
            "a&b",
            "a\nb",
            "a\0b",
            "a\u{1b}[0m",
            "caf\u{e9}",
            long.as_str(),
        ] {
            assert!(check_service_name(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn the_default_source_is_the_committed_head() {
        assert_eq!(
            Source::default(),
            Source::Commit {
                revision: "HEAD".to_owned()
            }
        );
    }

    /// An unknown field is a different protocol, not a field to ignore.
    #[test]
    fn unknown_fields_are_refused() {
        let json = br#"{"version":1,"request":{"op":"status","project":"/p"},"extra":1}
"#;
        let result: Result<Option<Envelope>, _> = read_frame(&mut json.as_slice());
        assert!(matches!(result, Err(WireError::Malformed)));
    }

    #[test]
    fn an_unknown_operation_is_refused() {
        let json = br#"{"version":1,"request":{"op":"destroy_everything","project":"/p"}}
"#;
        let result: Result<Option<Envelope>, _> = read_frame(&mut json.as_slice());
        assert!(matches!(result, Err(WireError::Malformed)));
    }

    #[test]
    fn an_error_message_is_escaped_on_the_way_in() {
        let error = ApiError::new(ErrorCode::Refused, "bad \u{1b}[31mthing\u{202e}");
        assert!(!error.message.contains('\u{1b}'));
        assert!(!error.message.contains('\u{202e}'));
    }

    #[test]
    fn an_error_message_cannot_fill_the_screen() {
        let error = ApiError::new(ErrorCode::Internal, "x".repeat(100_000));
        assert!(error.message.chars().count() < 2100);
    }
}
