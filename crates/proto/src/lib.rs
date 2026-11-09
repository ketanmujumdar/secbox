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

/// The progress step sent while the daemon waits for the person at this
/// computer to confirm an action in the system's own prompt. Its detail is how
/// they are being asked, such as "Touch ID or your login password". A client
/// shows that it is waiting and does nothing else: the answer is not its to give.
pub const STEP_CONFIRM: &str = "confirm";
/// The progress step sent when this computer has no way to ask the person, so
/// the action goes ahead on the client's request alone.
pub const STEP_UNCONFIRMED: &str = "unconfirmed";

/// The line a client shows for [`STEP_CONFIRM`] or [`STEP_UNCONFIRMED`], or
/// `None` for any other step. The same words everywhere, so the person learns
/// one sentence: a prompt they did not expect means something else asked.
pub fn person_step_line(step: &str, detail: Option<&str>) -> Option<String> {
    match step {
        STEP_CONFIRM => Some(format!(
            "Confirm with {} in the system prompt. Check it names what you asked for; if you did not ask, cancel it.",
            text::escape(detail.unwrap_or("the system prompt"))
        )),
        STEP_UNCONFIRMED => Some(
            "This computer has no fingerprint or password check, so this goes ahead on this request alone.".to_owned(),
        ),
        _ => None,
    }
}

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
    /// Remove a project from this machine: its sandbox, its records and, for an
    /// imported project, the daemon's copy. Never a folder of the person's own.
    Remove(ProjectArgs),
    /// Let a project's sandbox reach one more hostname, because a person said so.
    /// A sandbox has no way to send this: the socket is on the host, and no agent
    /// tool reaches it.
    ApproveHost(ApproveHostArgs),
    /// Report on a project's sandbox.
    Status(ProjectArgs),
    /// Read what a project's services have logged. Never starts anything.
    Logs(LogsArgs),
    /// Ask how to open a terminal in a running sandbox. Never starts anything.
    Terminal(TerminalArgs),
    /// Read the blocked hostnames recorded for a project. Never starts anything.
    Blocked(BlockedArgs),
    /// Compare what is in a running sandbox's `/work` with the snapshot it
    /// started from. Never starts anything, and nothing leaves the sandbox
    /// except the bundle the host reviews.
    Diff(ProjectArgs),
    /// Find, and if asked fetch, a project that lives in a Git remote over SSH.
    /// The daemon fetches with the user's SSH key on the host and keeps the
    /// files in its own state folder; nothing about the key reaches a sandbox.
    Fetch(FetchArgs),
    /// Run Git in a project's sandbox. Pull and push go through the daemon's
    /// relay, which reaches only the project's own remote, with the user's SSH
    /// key on the host, and checks every push before it leaves.
    Git(GitArgs),
    /// A project's secret names: the stored ones and where they go, and the
    /// empty ones still waiting for a value. Never a value, and the keychain
    /// is not read.
    Secrets(ProjectArgs),
    /// Store one secret's value in the keychain and choose the services that
    /// get it. Write-only: no request reads a value back.
    SetSecret(SetSecretArgs),
    /// The env files that could be used for secrets: those in a running
    /// sandbox, and those at the top of the project's folder on this machine.
    SecretFiles(ProjectArgs),
    /// The names in one env file, and what using it would do to each. Never a
    /// value.
    ReadSecretFile(SecretFileArgs),
    /// Stores the values of the names a person agreed to, from one env file,
    /// and gives them to the chosen services.
    UseSecretFile(UseSecretFileArgs),
    /// The limits a person chose for a project, what the project asks for, and
    /// how much this machine can spare. Changes nothing.
    ResourceChoices(ProjectArgs),
    /// Keeps a person's limits for a project, on the host. Nothing changes in a
    /// sandbox until it next starts, from a plan that shows them. A sandbox has
    /// no way to send this: the socket is on the host, and no agent tool reaches
    /// it.
    SetResources(SetResourcesArgs),
    /// The ports a project publishes, which are open, on which number of this
    /// Mac, and what the next start would change. Changes nothing.
    PortChoices(ProjectArgs),
    /// Keeps a person's choice for one port: open or closed, and the number it
    /// uses on this Mac. Nothing changes in a sandbox until it next starts, and
    /// that start asks the person to confirm, naming the ports it opens.
    SetPort(SetPortArgs),
    /// The hostnames a project's sandbox may reach: those its plan brings and
    /// those a person approved. Changes nothing.
    Hosts(ProjectArgs),
    /// Takes back a person's approval of one hostname. It only narrows what
    /// the sandbox can reach, so nobody is asked to confirm. A sandbox that
    /// exists is replaced without the host, keeping its data, because the
    /// engine cannot change a sandbox's network rules.
    BlockHost(ApproveHostArgs),
}

/// A CPU limit in words: "1 CPU", "1.5 CPUs", "2 CPUs".
pub fn cpus_text(millicpus: u32) -> String {
    let cpus = f64::from(millicpus) / 1000.0;
    format!("{cpus} {}", if millicpus == 1000 { "CPU" } else { "CPUs" })
}

/// Limits a person chose for one project. An absent value leaves it to the
/// project's own request, or the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceChoice {
    /// Also the limit while building: the person asked for one size of VM.
    #[serde(default)]
    pub memory_bytes: Option<u64>,
    #[serde(default)]
    pub millicpus: Option<u32>,
    #[serde(default)]
    pub disk_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetResourcesArgs {
    pub project: String,
    pub choice: ResourceChoice,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetPortArgs {
    pub project: String,
    /// The port as the project publishes it, which names it.
    pub port: u16,
    pub open: bool,
    /// The number to use on this Mac. `None` keeps the one chosen before, or
    /// the project's own number.
    #[serde(default)]
    pub mac_port: Option<u16>,
}

/// One port a project publishes, and what a person chose for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortChoice {
    pub service: String,
    pub container_port: u16,
    /// The number the project publishes. It names the port.
    pub port: u16,
    /// The number the next start uses on this Mac's `127.0.0.1`.
    pub mac_port: u16,
    /// Whether the next start opens it.
    pub open: bool,
    /// The number it is open on now. `None` when it is closed, or there is no
    /// sandbox.
    pub open_now: Option<u16>,
    /// Why it cannot be opened on this sandbox, if it cannot.
    pub cannot_open: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortChoicesReport {
    pub project_id: String,
    pub ports: Vec<PortChoice>,
    /// Why the ports could not be read from the project's files, if they could not.
    pub problem: Option<String>,
    /// True when it is running with ports other than the next start's.
    pub restart_needed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceChoicesReport {
    pub project_id: String,
    pub chosen: ResourceChoice,
    /// What the sandbox has now. `None` when there is no sandbox.
    pub current: Option<ResourcePlan>,
    /// What the next start would use. `None` when this machine could not spare
    /// it, and `problem` says why.
    pub next: Option<ResourcePlan>,
    pub problem: Option<String>,
    /// True when it is running with limits other than `next`.
    pub restart_needed: bool,
    /// What this machine can spare now, after what it keeps for itself.
    pub max_memory_bytes: u64,
    pub max_millicpus: u32,
    pub max_disk_bytes: u64,
}

/// What a running sandbox is using. Numbers to show, never to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxUsage {
    /// CPU in use, where 1000 is one whole CPU.
    pub cpu_millis: u32,
    pub memory_used_bytes: u64,
    /// The size of the VM's memory, which can be above the run limit when the
    /// build limit is larger.
    pub memory_limit_bytes: u64,
    /// What this machine holds for the VM.
    pub memory_host_bytes: u64,
    /// What the guest's writes take on this machine's disk.
    pub disk_used_bytes: Option<u64>,
}

/// Where an env file is. The sandbox's copy is hostile data, like anything
/// else read from a guest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "where", content = "path")]
pub enum SecretFileSource {
    /// Relative to the sandbox's `/work`.
    Sandbox(String),
    /// An absolute path on this machine.
    Host(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretFileArgs {
    pub project: String,
    pub source: SecretFileSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UseSecretFileArgs {
    pub project: String,
    pub source: SecretFileSource,
    /// The names the person saw and agreed to. The file is read again to use
    /// it, and a name added since then is left out rather than stored unseen.
    pub names: Vec<String>,
    /// Who gets them. Each must be a service in the project's Compose file.
    /// Empty stores them and delivers them nowhere.
    pub services: Vec<String>,
}

/// One secret to store. The value is a [`SecretText`], so it never shows in
/// debug output and its memory is wiped when it is dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetSecretArgs {
    pub project: String,
    pub name: String,
    pub value: SecretText,
    /// The project's services that may receive it. Each must be one of the
    /// services in the project's own Compose file. Empty stores the value and
    /// delivers it nowhere.
    pub services: Vec<String>,
}

/// The longest secret value a client may send.
pub const MAX_SECRET_BYTES: usize = 64 * 1024;

/// A secret value on its way to the daemon. Its `Debug` is redacted, and its
/// buffer is zeroed on drop. Serde copies through it, so a client should drop
/// its own copies as soon as the request is written.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretText(String);

impl SecretText {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretText([redacted])")
    }
}

impl Drop for SecretText {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitArgs {
    pub project: String,
    /// Named `action` because `op` already names the request on the wire.
    pub action: GitOp,
    /// Who commits and merges in the sandbox. The command line fills it from
    /// the person's own Git settings. Without one, a fixed sandbox name is used.
    #[serde(default)]
    pub identity: Option<GitIdentity>,
    /// As for [`FetchArgs::ssh_auth_sock`].
    #[serde(default)]
    pub ssh_auth_sock: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "git", rename_all = "snake_case", deny_unknown_fields)]
pub enum GitOp {
    /// The branch, what changed, and how far it is from the remote.
    Status,
    /// Fetch and merge the current branch's upstream.
    Pull,
    /// Stage and commit. With no paths, everything that changed.
    Commit { message: String, paths: Vec<String> },
    /// Push the current branch to the same name on the remote.
    Push,
    /// Any Git command, as its arguments after `git`, such as
    /// `["switch", "-c", "fix"]`. It runs inside the sandbox. `fetch`, `pull`
    /// and `ls-remote` go through the relay as a fetch, `push` as a push with
    /// the usual checks, and every other command gets no relay at all.
    Run { args: Vec<String> },
}

/// The most arguments one [`GitOp::Run`] may have.
pub const MAX_GIT_ARGS: usize = 64;
/// The longest one argument may be. Room for a commit message given with `-m`.
pub const MAX_GIT_ARG_BYTES: usize = MAX_COMMIT_MESSAGE_BYTES;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitIdentity {
    pub name: String,
    pub email: String,
}

/// The longest commit message accepted.
pub const MAX_COMMIT_MESSAGE_BYTES: usize = 16 * 1024;

/// Checks a name or email before it goes on Git's command line in a sandbox.
pub fn check_identity(identity: &GitIdentity) -> Result<(), &'static str> {
    for (value, what) in [(&identity.name, "name"), (&identity.email, "email")] {
        if value.trim().is_empty() || value.len() > 200 {
            return Err(if what == "name" { "the Git name must be 1 to 200 characters" } else { "the Git email must be 1 to 200 characters" });
        }
        if value.chars().any(|c| c.is_control() || matches!(c, '<' | '>')) {
            return Err("a Git name or email must not hold control characters or angle brackets");
        }
    }
    Ok(())
}

/// One branch a push changed, or tried to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefUpdate {
    pub name: String,
    pub old: String,
    pub new: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitReport {
    /// Git exited with 0 and nothing was refused.
    pub ok: bool,
    /// Git's exit code inside the sandbox, if it got that far.
    pub code: Option<i32>,
    /// What Git printed, filtered, and cut at a limit.
    pub output: String,
    /// The branch checked out after the operation, if there is one.
    pub branch: Option<String>,
    /// For a push: the branch changes it carried, checked by the daemon.
    pub updates: Vec<RefUpdate>,
    /// Why the daemon stopped this, if it did.
    pub refused: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchArgs {
    /// An SSH remote: `git@host:org/repo.git` or `ssh://...`.
    pub url: String,
    /// A branch, tag or full commit id. `None` means the remote's default branch.
    pub git_ref: Option<String>,
    /// Fetch now. Without it, only an earlier fetch is looked up.
    pub fetch: bool,
    /// The client's SSH agent socket, which may be newer than the one the
    /// daemon started with. The daemon checks it is a socket owned by its own
    /// user, and passes it to `ssh` and nothing else.
    #[serde(default)]
    pub ssh_auth_sock: Option<String>,
}

/// Where a remote project's files are, and what they are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchReport {
    pub url: String,
    /// The folder the daemon keeps the project in. Every other request names
    /// the project by this path.
    pub project_path: String,
    /// The commit the files come from, if it could be read.
    pub commit: Option<String>,
    /// What was asked for, or `HEAD`.
    pub git_ref: String,
    /// True if this request fetched. False when a sandbox already holds an
    /// earlier fetch, which is never replaced under it, or when only a lookup
    /// was asked for.
    pub fetched: bool,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockedArgs {
    pub project: String,
    /// Only what was written after this point, from an earlier report's `next`.
    pub after: Option<BlockedCursor>,
}

/// A place in the daemon's record of blocked names, for following it. Opaque to
/// clients: the daemon that gave it is the only thing that can read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockedCursor {
    pub file: u64,
    pub offset: u64,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApproveHostArgs {
    pub project: String,
    pub host: String,
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
    Remove(RemoveReport),
    ApproveHost(ApproveHostReport),
    Status(StatusReport),
    Logs(LogsReport),
    Terminal(TerminalReport),
    Blocked(BlockedReport),
    Fetch(FetchReport),
    Git(GitReport),
    Diff(DiffReport),
    Secrets(SecretsReport),
    SetSecret(SetSecretReport),
    SecretFiles(SecretFilesReport),
    ReadSecretFile(SecretFilePreview),
    UseSecretFile(UseSecretFileReport),
    // Boxed for the same reason as a plan.
    ResourceChoices(Box<ResourceChoicesReport>),
    SetResources(Box<ResourceChoicesReport>),
    PortChoices(PortChoicesReport),
    SetPort(PortChoicesReport),
    Hosts(HostsReport),
    BlockHost(BlockHostReport),
}

/// What changed inside a sandbox since the snapshot it started from.
///
/// The host compared the two itself, from a bundle the sandbox produced, and
/// ran nothing from it. Every path and detail is escaped. The flags are a
/// heuristic aid for a person, not proof that the rest is safe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffReport {
    pub project_id: String,
    /// The commit the host made of the snapshot when the sandbox was created.
    pub baseline: String,
    /// What needs a person's attention, the most serious first.
    pub flags: Vec<DiffFlag>,
    /// Every changed path, in path order.
    pub files: Vec<DiffFile>,
    /// Commits looked at, including the one made of uncommitted work.
    pub commits: u64,
    /// False when a limit stopped the review, so the flags understate the risk.
    pub complete: bool,
}

impl DiffReport {
    pub fn flagged(&self) -> bool {
        !self.flags.is_empty()
    }
}

/// How a path changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChange {
    Added,
    Modified,
    Deleted,
    /// A file became a link, or the other way round.
    TypeChanged,
}

impl FileChange {
    pub fn word(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::TypeChanged => "type changed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffFile {
    /// Escaped. Quoted when it has spaces, quotes or escapes in it.
    pub path: String,
    pub change: FileChange,
    /// The lines that changed, or why they are not shown. A daemon that does
    /// not send this reads as not included.
    #[serde(default)]
    pub patch: FilePatch,
}

/// What a person can be shown of one changed file. Text in it was checked for
/// likely secrets first, and is escaped except for tabs.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FilePatch {
    Text {
        hunks: Vec<DiffHunk>,
        /// Lines added and removed in the whole file, even when cut short.
        added: u64,
        removed: u64,
        /// A limit cut the lines, so the rest of this file has not been shown.
        truncated: bool,
    },
    /// It looks like it holds a secret, so none of it is shown.
    Withheld,
    Binary,
    TooLarge,
    Submodule,
    Unreadable,
    /// The reply had no room for it, or the keychain could not be read, or the
    /// daemon is older than this field.
    #[default]
    NotIncluded,
}

impl FilePatch {
    /// Says why there are no lines to read. `None` for text.
    ///
    /// Every word of this is what a reviewer goes by, and a sandbox can bring
    /// about any of these on purpose, so each one says the file is unread.
    pub fn unread_reason(&self) -> Option<&'static str> {
        match self {
            Self::Text { .. } => None,
            Self::Withheld => Some("Not shown, because it looks like it holds a secret. A sandbox can cause this on purpose by planting a key-shaped string, so treat this file as unread."),
            Self::Binary => Some("Not shown, because it is a binary file. Treat it as unread."),
            Self::TooLarge => Some("Not shown, because it is too large. Treat it as unread."),
            Self::Submodule => Some("Not shown, because it is a submodule pointer to another repository. Treat what it points at as unread."),
            Self::Unreadable => Some("Not shown, because it could not be read. Treat it as unread."),
            Self::NotIncluded => Some("Not shown, because this comparison had no room for it or could not check it for secrets. Treat it as unread."),
        }
    }

    /// True when part or all of the file has not been shown to anyone.
    pub fn is_incomplete(&self) -> bool {
        match self {
            Self::Text { truncated, .. } => *truncated,
            _ => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffHunk {
    /// One-based first line in the old file, or 0 when the hunk has none.
    pub old_start: u64,
    /// One-based first line in the new file, or 0 when the hunk has none.
    pub new_start: u64,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
}

/// One finding. It never holds file content, so never a secret's value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffFlag {
    /// A stable name for the kind of finding, such as `agent_hook`.
    pub kind: String,
    /// The path it is about, or `None` for a finding about the whole change.
    pub path: Option<String>,
    /// A line in the new file, for a likely secret.
    pub line: Option<u64>,
    /// What it means, in words.
    pub message: String,
    /// Only in a commit made inside the sandbox, not in the end result. An
    /// export would still carry that commit.
    pub earlier_commit: bool,
}

/// Whether a project's sandbox is writing down what it is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockedRecording {
    /// The host's record says it was created at a level that writes them.
    Recording,
    /// It exists, and either its level writes nothing or no level is on record.
    /// An empty list from this sandbox would say nothing at all.
    NotRecording,
    NoSandbox,
}

/// What can be said about who could have written the record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogTrust {
    /// Nothing was read, so there is nothing to say.
    NotRead,
    /// Only the host's engine writes it, as far as was measured and checked.
    HostOnly,
    /// Something else could have written it, for this reason. Do not believe it.
    Doubtful(String),
}

/// One hostname and how often it was refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockedEvent {
    /// When it was last refused, in UTC, as the engine wrote it.
    pub time: String,
    /// A name in DNS text form, already checked and safe to print. The guest
    /// chose it, so it is a thing that was asked for and proves nothing else.
    pub domain: String,
    pub count: u64,
    /// The sandbox as it is now may reach this name, because it was approved
    /// after these refusals. They are history.
    #[serde(default)]
    pub allowed: bool,
}

/// Blocked hostnames only: most blocked traffic leaves no record at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockedReport {
    pub recording: BlockedRecording,
    /// The level on the host's record, if there is one.
    pub log_level: Option<String>,
    /// The newest last.
    pub events: Vec<BlockedEvent>,
    /// Every refusal counted, including any in `omitted_domains`.
    pub total: u64,
    /// Older events exist or may exist that this did not see.
    pub cut_off: bool,
    /// How many of the oldest names were left out of `events`.
    pub omitted_domains: u64,
    /// Lines in the place of an event that failed the check, so were not used.
    pub rejected_lines: u64,
    /// Names counted before the daemon last started. The guest chooses a name,
    /// so its text is held in memory only and these can no longer be shown.
    #[serde(default)]
    pub unlisted_domains: u64,
    pub trust: LogTrust,
    /// Where to ask from next, to get only what is new. It means nothing
    /// outside the daemon that gave it.
    pub next: Option<BlockedCursor>,
}

/// The short form of [`BlockedReport`], for `status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockedSummary {
    pub recording: BlockedRecording,
    pub total: u64,
    pub domains: u64,
    pub cut_off: bool,
    pub trust: LogTrust,
    /// Why the log could not be read, when it could not.
    pub problem: Option<String>,
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

/// Why a request did not succeed. The message is already safe to print: only
/// its line breaks are left, as a build's last lines need them.
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
            message: text::escape_keeping_breaks(message.as_ref(), 2000),
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
    /// The Compose file the host wrote for a project that has only a
    /// Dockerfile, exactly as it will be used. It is part of the plan so that
    /// the digest, and so the approval, covers it. `None` for a Compose
    /// project, which runs its own files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_compose: Option<String>,
}

/// A port the project asks to publish. Every one is forwarded to `127.0.0.1`
/// only, so there is no bind address here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortPlan {
    pub service: String,
    /// The number the project publishes. It names the port in an approval.
    pub host_port: u16,
    pub container_port: u16,
    /// The number used on this Mac. The same as `host_port` unless a person
    /// chose another. `0` from an older daemon means the same.
    #[serde(default)]
    pub mac_port: u16,
    /// Whether a person chose to open it, for a project where they have chosen.
    /// `None` when nothing was chosen yet.
    #[serde(default)]
    pub chosen: Option<bool>,
    /// The project published this on every interface. Inside the sandbox that
    /// is harmless; on the host it is still only `127.0.0.1`.
    pub was_all_interfaces: bool,
    /// A variable can change this at start time, so the number here is a guess
    /// from the file's defaults.
    pub dynamic: bool,
}

impl PortPlan {
    /// The number used on this Mac.
    pub fn on_mac(&self) -> u16 {
        if self.mac_port == 0 {
            self.host_port
        } else {
            self.mac_port
        }
    }
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

/// A hostname a person approved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApproveHostReport {
    /// As stored: lowercase, with no trailing dot.
    pub host: String,
    /// What the sandbox is now.
    pub sandbox: SandboxState,
    /// The sandbox was replaced, so the host is reachable now. If not, there is
    /// no sandbox yet, or the host was already allowed.
    pub applied: bool,
    pub already_allowed: bool,
    pub seconds: u64,
}

/// The hostnames a project's sandbox may reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostsReport {
    pub sandbox: SandboxState,
    /// From the project's plan: the hosts its build needs. Not approved by
    /// anyone and not removable here.
    pub plan_hosts: Vec<String>,
    /// The plan's hosts stay reachable while it runs, not only while it builds.
    pub plan_hosts_while_running: bool,
    /// Why the plan's hosts could not be listed, if they could not.
    pub plan_note: Option<String>,
    /// Approved by a person, sorted by name.
    pub approved: Vec<ApprovedHost>,
    /// Hosts the sandbox can still reach that are no longer approved or in the
    /// plan. They go when it is next replaced.
    pub leaving: Vec<String>,
}

/// One hostname a person approved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedHost {
    pub host: String,
    /// When it was approved. None for an approval older than this record.
    pub approved_unix: Option<u64>,
    /// The sandbox as it is can reach it. False when there is no sandbox yet.
    pub in_effect: bool,
}

/// A hostname whose approval was taken back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockHostReport {
    /// As stored: lowercase, with no trailing dot.
    pub host: String,
    /// What the sandbox is now.
    pub sandbox: SandboxState,
    /// The sandbox was replaced, so the host is blocked now. If not, there is
    /// no sandbox yet, or the host was not approved.
    pub applied: bool,
    /// It was approved. If not, nothing changed.
    pub was_approved: bool,
    pub seconds: u64,
}

/// A project's secrets, by name. Never a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsReport {
    pub project_id: String,
    /// Stored and empty names together, sorted by name.
    pub secrets: Vec<SecretEntry>,
    /// The services in the project's Compose file, which are the only ones a
    /// secret can be given to.
    pub services: Vec<String>,
    /// Why `services` is empty when the project's files could not be read.
    pub services_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretEntry {
    pub name: String,
    /// False for a name with no value yet, such as one from a `.env.example`.
    pub stored: bool,
    /// The services it is given to. Empty for none.
    pub services: Vec<String>,
    /// How it reaches them, in words, such as "injected into the environment".
    pub route: Option<String>,
}

/// What storing a secret did. Names only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetSecretReport {
    pub name: String,
    /// True if it replaced a value that was already stored.
    pub replaced: bool,
    pub services: Vec<String>,
    /// True if the project's sandbox is running, so the new value reaches it
    /// only on the next start.
    pub applies_on_next_start: bool,
}

/// Env files that could be used for secrets. Every path is escaped for
/// showing. One that cannot be shown exactly is left out, because what is
/// shown here is sent back to name the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretFilesReport {
    /// Relative to the sandbox's `/work`. The sandbox could have made these up.
    pub sandbox: Vec<String>,
    /// Absolute paths on this machine.
    pub host: Vec<String>,
    /// Why the sandbox was not looked in, or what stopped it.
    pub note: Option<String>,
}

/// The names in one env file. Never a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretFilePreview {
    pub source: SecretFileSource,
    pub names: Vec<SecretFileName>,
    /// The file is a template such as `.env.example`: only its names are used,
    /// and its values are dropped unread.
    pub names_only: bool,
    /// The services a secret can be given to.
    pub services: Vec<String>,
    pub services_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretFileName {
    pub name: String,
    /// What using the file would do with this one.
    pub effect: SecretFileEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretFileEffect {
    /// Stored, with nothing there before.
    Store,
    /// Stored over a value already in the keychain, which is then gone.
    Replace,
    /// No value here, so the name is added to fill in later.
    Empty,
    /// No value here, and one is already stored, which stays.
    Keep,
    /// A line break can't go in an environment, so it would stop every start.
    Skip,
}

/// What using an env file did. Names only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UseSecretFileReport {
    pub stored: Vec<String>,
    pub replaced: Vec<String>,
    /// Added with no value, to fill in later.
    pub empty: Vec<String>,
    /// Left out, each with why.
    pub skipped: Vec<SkippedSecret>,
    pub services: Vec<String>,
    /// The sandbox is running, so the values reach it on its next start.
    pub applies_on_next_start: bool,
}

impl UseSecretFileReport {
    /// The same words for every client. Names only.
    pub fn summary(&self) -> String {
        let list = |names: &[String]| {
            names.iter().map(|n| text::escape(n)).collect::<Vec<_>>().join(", ")
        };
        let mut parts = Vec::new();
        let count = self.stored.len() + self.replaced.len();
        if count > 0 {
            let goes = if self.services.is_empty() {
                "They go to no service yet.".to_owned()
            } else {
                format!("They go to {}.", list(&self.services))
            };
            parts.push(format!("Stored {count} in your keychain. {goes}"));
        }
        if !self.replaced.is_empty() {
            parts.push(format!("Replaced {}.", list(&self.replaced)));
        }
        if !self.empty.is_empty() {
            parts.push(format!("No value for {}, so these wait to be filled in.", list(&self.empty)));
        }
        for skipped in &self.skipped {
            parts.push(format!("Left out {}: {}.", text::escape(&skipped.name), skipped.reason));
        }
        if parts.is_empty() {
            parts.push("Nothing was changed.".to_owned());
        }
        if count > 0 && self.applies_on_next_start {
            parts.push("The running sandbox gets them on its next start.".to_owned());
        }
        parts.join(" ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkippedSecret {
    pub name: String,
    pub reason: String,
}

/// What removing a project took with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoveReport {
    pub project_id: String,
    /// There was a sandbox, and it is gone.
    pub sandbox_destroyed: bool,
    /// The project was imported, and the daemon's copy is gone.
    pub removed_copy: bool,
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
    /// Blocked hostname lookups the host's log holds for this project. Raw IP,
    /// metadata and UDP attempts are never in it, so zero proves nothing.
    pub blocked_count: u64,
    /// `None` from a daemon too old to send it, which is not the same as zero.
    #[serde(default)]
    pub blocked: Option<BlockedSummary>,
    /// Which secrets this project's services are given, and every host the
    /// sandbox can reach while it runs. `None` from an older daemon.
    #[serde(default)]
    pub exposure: Option<Box<SecretExposure>>,
    /// What it is using now. `None` when it is not running, when the engine
    /// could not say, or from an older daemon.
    #[serde(default)]
    pub usage: Option<Box<SandboxUsage>>,
}

/// Where a project's secrets could be sent. Names only, never a value.
///
/// A service that holds a secret can send it to any host the sandbox can
/// reach, and the network rules are per sandbox, not per service. So the
/// honest answer to "where can this key go" is this whole list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretExposure {
    pub secrets: Vec<HeldSecret>,
    /// Every host reachable while it runs, build hosts included when they
    /// stay open. `None` for a sandbox created before this was recorded.
    pub reachable_hosts: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeldSecret {
    pub name: String,
    pub services: Vec<String>,
}

/// A time in UTC to the minute, such as `2026-11-08 04:41 UTC`.
pub fn utc_minute(unix: u64) -> String {
    let days = i64::try_from(unix / 86_400).unwrap_or(i64::MAX / 2);
    let secs = unix % 86_400;
    // Days to a civil date, after Howard Hinnant's `civil_from_days`.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02} UTC", secs / 3600, secs % 3600 / 60)
}

/// Hosts the sandbox planned here can reach while it runs.
pub fn reachable_while_running(network: &NetworkPlan) -> Vec<String> {
    let mut hosts: Vec<String> = network.run_hosts.clone();
    if network.merged_for_whole_life {
        hosts.extend(network.build_hosts.iter().cloned());
    }
    hosts.sort();
    hosts.dedup();
    hosts
}

/// The same words in every client: which secret is held by which service,
/// and where it could be sent. Empty when no secret is assigned.
/// Hosts on the build menu that accept uploads to an account anyone can
/// make: a bucket, an image repository, a package.
pub const UPLOADS_FROM_ANYONE: [&str; 3] = [
    "registry-1.docker.io",
    "registry.npmjs.org",
    "storage.googleapis.com",
];

/// The secrets a plan would deliver, as [`HeldSecret`]s.
pub fn held_from_plan(secrets: &[SecretPlan]) -> Vec<HeldSecret> {
    secrets
        .iter()
        .map(|s| HeldSecret {
            name: s.name.clone(),
            services: s.services.clone(),
        })
        .collect()
}

/// With `one_line`, the hosts are joined on one line for a narrow box.
pub fn exposure_lines(secrets: &[HeldSecret], hosts: Option<&[String]>, one_line: bool) -> Vec<String> {
    if secrets.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<String> = secrets
        .iter()
        .map(|s| {
            let to = if s.services.is_empty() {
                "no service".to_owned()
            } else {
                s.services.join(", ")
            };
            format!("{} is given to {}", s.name, to)
        })
        .collect();
    match hosts {
        None => lines.push(
            "Where they could be sent is not recorded for this sandbox. Recreate it to see.".to_owned(),
        ),
        Some([]) => lines.push(
            "They cannot be sent anywhere: every outgoing connection is refused.".to_owned(),
        ),
        Some(hosts) => {
            lines.push(match hosts.len() {
                1 => "Code in those services could send them to this host, and nowhere else:".to_owned(),
                n => format!("Code in those services could send them to any of these {n} hosts, and nowhere else:"),
            });
            if one_line {
                lines.push(hosts.join(", "));
            } else {
                lines.extend(hosts.iter().map(|h| format!("  {h}")));
            }
            let open: Vec<&str> = UPLOADS_FROM_ANYONE
                .iter()
                .copied()
                .filter(|h| hosts.iter().any(|x| x == h))
                .collect();
            if !open.is_empty() {
                let verb = if open.len() == 1 { "accepts" } else { "accept" };
                lines.push(format!(
                    "Of these, {} {verb} uploads to an account anyone can make, so a stolen value could be sent there.",
                    open.join(", ")
                ));
            }
        }
    }
    lines
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
mod secret_text_tests {
    use super::*;

    #[test]
    fn a_secret_value_never_shows_in_debug_output() {
        let args = SetSecretArgs {
            project: "/p".to_owned(),
            name: "API_KEY".to_owned(),
            value: SecretText::new("hunter2-sentinel".to_owned()),
            services: vec![],
        };
        let shown = format!("{:?}", Envelope::new(Request::SetSecret(args)));
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("redacted"), "{shown}");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_time_is_shown_in_utc_to_the_minute() {
        assert_eq!(super::utc_minute(0), "1970-01-01 00:00 UTC");
        assert_eq!(super::utc_minute(1_794_113_048), "2026-11-08 04:44 UTC");
        assert_eq!(super::utc_minute(951_782_400), "2000-02-29 00:00 UTC");
    }

    use super::*;

    #[test]
    fn one_cpu_is_singular_and_any_other_amount_is_not() {
        assert_eq!(cpus_text(1000), "1 CPU");
        assert_eq!(cpus_text(500), "0.5 CPUs");
        assert_eq!(cpus_text(1500), "1.5 CPUs");
        assert_eq!(cpus_text(2000), "2 CPUs");
    }

    #[test]
    fn exposure_names_every_reachable_host_and_never_says_nowhere_when_unknown() {
        let held = vec![HeldSecret { name: "DEMO_TOKEN".into(), services: vec!["api".into()] }];
        let hosts = vec!["a.example".to_owned(), "b.example".to_owned()];
        let lines = exposure_lines(&held, Some(&hosts), false);
        assert_eq!(lines[0], "DEMO_TOKEN is given to api");
        assert!(lines.iter().any(|l| l.contains("these 2 hosts")));
        assert!(lines.iter().any(|l| l.trim() == "b.example"));
        // Neither host takes uploads from anyone, so nothing is said about it.
        assert!(!lines.iter().any(|l| l.contains("uploads")));
        let npm = vec!["registry.npmjs.org".to_owned()];
        let warned = exposure_lines(&held, Some(&npm), false);
        assert!(warned.iter().any(|l| l.contains("registry.npmjs.org accepts uploads")), "{warned:?}");
        assert!(warned.iter().any(|l| l.contains("send them to this host, and nowhere else")), "{warned:?}");
        let none = exposure_lines(&held, Some(&[]), false);
        assert!(none.iter().any(|l| l.contains("cannot be sent anywhere")));
        let unknown = exposure_lines(&held, None, false);
        assert!(!unknown.iter().any(|l| l.contains("cannot be sent")));
        assert!(exposure_lines(&[], Some(&hosts), false).is_empty());
    }

    #[test]
    fn build_hosts_count_as_reachable_only_when_they_stay_open() {
        let mut network = NetworkPlan {
            build_hosts: vec!["registry.npmjs.org".into()],
            run_hosts: vec!["api.stripe.com".into()],
            tls: TlsPlan::ServerNameOnly,
            merged_for_whole_life: true,
        };
        assert_eq!(reachable_while_running(&network), vec!["api.stripe.com", "registry.npmjs.org"]);
        network.merged_for_whole_life = false;
        assert_eq!(reachable_while_running(&network), vec!["api.stripe.com"]);
    }

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
        round_trip(Request::Blocked(BlockedArgs {
            project: "/p".to_owned(),
            after: Some(BlockedCursor { file: 7, offset: 9 }),
        }));
        round_trip(Request::Diff(ProjectArgs {
            project: "/p".to_owned(),
        }));
        round_trip(Request::Secrets(ProjectArgs {
            project: "/p".to_owned(),
        }));
        round_trip(Request::SetSecret(SetSecretArgs {
            project: "/p".to_owned(),
            name: "API_KEY".to_owned(),
            value: SecretText::new("v".to_owned()),
            services: vec!["api".to_owned()],
        }));
        round_trip(Request::SecretFiles(ProjectArgs {
            project: "/p".to_owned(),
        }));
        round_trip(Request::ReadSecretFile(SecretFileArgs {
            project: "/p".to_owned(),
            source: SecretFileSource::Sandbox(".env".to_owned()),
        }));
        round_trip(Request::UseSecretFile(UseSecretFileArgs {
            project: "/p".to_owned(),
            source: SecretFileSource::Host("/home/me/.env".to_owned()),
            names: vec!["API_KEY".to_owned()],
            services: vec!["api".to_owned()],
        }));
        round_trip(Request::Git(GitArgs {
            project: "/p".to_owned(),
            action: GitOp::Commit { message: "m".to_owned(), paths: vec!["a".to_owned()] },
            identity: Some(GitIdentity { name: "A".to_owned(), email: "a@example.invalid".to_owned() }),
            ssh_auth_sock: None,
        }));
        round_trip(Request::Fetch(FetchArgs {
            url: "git@example.invalid:o/r.git".to_owned(),
            git_ref: Some("main".to_owned()),
            fetch: true,
            ssh_auth_sock: Some("/tmp/agent.sock".to_owned()),
        }));
    }

    #[test]
    fn git_and_fetch_replies_survive_the_wire() {
        for reply in [
            Reply::Git(GitReport {
                ok: false,
                code: Some(1),
                output: "a\nb".to_owned(),
                branch: Some("feature".to_owned()),
                updates: vec![RefUpdate { name: "refs/heads/f".to_owned(), old: "0".repeat(40), new: "1".repeat(40) }],
                refused: Some("why".to_owned()),
            }),
            Reply::Fetch(FetchReport {
                url: "git@h:o/r.git".to_owned(),
                project_path: "/p".to_owned(),
                commit: None,
                git_ref: "HEAD".to_owned(),
                fetched: true,
            }),
        ] {
            let frame = Frame::Done(Outcome::Ok { reply });
            let mut buffer = Vec::new();
            write_frame(&mut buffer, &frame).unwrap();
            let back: Frame = read_frame(&mut buffer.as_slice()).unwrap().unwrap();
            assert_eq!(back, frame);
        }
        for op in [GitOp::Status, GitOp::Pull, GitOp::Push, GitOp::Run { args: vec!["log".to_owned(), "--oneline".to_owned()] }] {
            round_trip(Request::Git(GitArgs { project: "/p".to_owned(), action: op, identity: None, ssh_auth_sock: None }));
        }
    }

    #[test]
    fn a_diff_report_survives_the_wire() {
        let frame = Frame::Done(Outcome::Ok {
            reply: Reply::Diff(DiffReport {
                project_id: "p".to_owned(),
                baseline: "a".repeat(40),
                flags: vec![DiffFlag {
                    kind: "agent_hook".to_owned(),
                    path: Some(".claude/settings.json".to_owned()),
                    line: None,
                    message: "adds a hook".to_owned(),
                    earlier_commit: false,
                }],
                files: vec![DiffFile {
                    path: ".claude/settings.json".to_owned(),
                    change: FileChange::Added,
                    patch: FilePatch::NotIncluded,
                }],
                commits: 1,
                complete: true,
            }),
        });
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &frame).unwrap();
        let back: Frame = read_frame(&mut buffer.as_slice()).unwrap().unwrap();
        assert_eq!(back, frame);
    }

    /// The request is an addition: its wire form is the operation's name and
    /// the project, and nothing about the requests before it changed.
    #[test]
    fn the_diff_request_is_named_on_the_wire() {
        let json = br#"{"version":1,"request":{"op":"diff","project":"/p"}}
"#;
        let envelope: Envelope = read_frame(&mut json.as_slice()).unwrap().unwrap();
        assert_eq!(
            envelope.request,
            Request::Diff(ProjectArgs {
                project: "/p".to_owned()
            })
        );
    }

    #[test]
    fn a_blocked_report_survives_the_wire() {
        let frame = Frame::Done(Outcome::Ok {
            reply: Reply::Blocked(BlockedReport {
                recording: BlockedRecording::Recording,
                log_level: Some("debug".to_owned()),
                events: vec![BlockedEvent {
                    time: "2026-11-05T07:50:52.406636Z".to_owned(),
                    domain: "a.example.net".to_owned(),
                    count: 2,
                    allowed: false,
                }],
                total: 2,
                cut_off: true,
                omitted_domains: 1,
                rejected_lines: 1,
                unlisted_domains: 0,
                trust: LogTrust::Doubtful("why".to_owned()),
                next: Some(BlockedCursor { file: 1, offset: 2 }),
            }),
        });
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &frame).unwrap();
        let back: Frame = read_frame(&mut buffer.as_slice()).unwrap().unwrap();
        assert_eq!(back, frame);
    }

    /// A daemon started before this field existed still has to be understood, or
    /// every client would fail on `status` until the daemon was restarted.
    #[test]
    fn a_status_from_an_older_daemon_is_still_understood() {
        let json = br#"{"frame":"done","outcome":"ok","reply":{"reply":"status","project_id":"p","project_path":"/p","sandbox":"running","services":[],"ports":[],"resources":null,"blocked_count":0}}
"#;
        let frame: Frame = read_frame(&mut json.as_slice()).unwrap().unwrap();
        let Frame::Done(Outcome::Ok { reply: Reply::Status(report) }) = frame else {
            panic!("expected a status");
        };
        assert_eq!(report.blocked, None);
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

    #[test]
    fn the_person_steps_have_one_wording_and_others_have_none() {
        let asking = person_step_line(STEP_CONFIRM, Some("Touch ID or your login password")).unwrap();
        assert!(asking.starts_with("Confirm with Touch ID or your login password"), "{asking}");
        assert!(asking.contains("cancel it"));
        let hostile = person_step_line(STEP_CONFIRM, Some("\x1b]0;evil\x07")).unwrap();
        assert!(!hostile.contains('\x1b') && !hostile.contains('\x07'), "{hostile:?}");
        assert!(person_step_line(STEP_UNCONFIRMED, None).unwrap().contains("no fingerprint"));
        assert_eq!(person_step_line("build", Some("web")), None);
    }
}
