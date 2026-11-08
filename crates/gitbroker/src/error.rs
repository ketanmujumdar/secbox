//! Errors. Any text that came from git or from a bundle is escaped first, so
//! printing an error cannot inject terminal control sequences.

use std::io;

/// All failures. Every variant fails closed: nothing was approved or pushed
/// unless the variant says otherwise.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("system git is unavailable: {0}")]
    GitUnavailable(String),
    #[error("git {found} is older than the verified minimum {required}")]
    GitTooOld { found: String, required: String },
    #[error("git {op} timed out after {seconds}s")]
    Timeout { op: &'static str, seconds: u64 },
    #[error("git {op} produced more than {limit} bytes of output")]
    OutputTooLarge { op: &'static str, limit: u64 },
    #[error("git {op} failed{}: {stderr}", exit_note(*.code))]
    GitFailed {
        op: &'static str,
        code: Option<i32>,
        stderr: String,
    },
    #[error("bundle path is not a regular file")]
    BundleNotRegularFile,
    #[error("bundle is {size} bytes, over the limit of {limit}")]
    BundleTooLarge { size: u64, limit: u64 },
    #[error("bundle is malformed: {0}")]
    BundleMalformed(&'static str),
    #[error("rejected ref name: {0}")]
    BadRefName(String),
    #[error("bundle rejected by git: {stderr}")]
    ImportRejected { stderr: String },
    #[error("not a full lowercase commit id: {0}")]
    InvalidCommitId(String),
    #[error("invalid task name: {0}")]
    InvalidTaskName(String),
    #[error("rejected remote: {0}")]
    InvalidRemote(&'static str),
    #[error("rejected ssh setup: {0}")]
    InvalidSsh(&'static str),
    #[error("rejected credential helper: {0}")]
    InvalidCredentialHelper(&'static str),
    #[error("commit {0} is not in quarantine")]
    CommitNotInQuarantine(String),
    #[error("path is not a quarantine: {0}")]
    NotAQuarantine(String),
    #[error("path already exists: {0}")]
    PathExists(String),
    #[error("approval is for commit {approved}, but the review is of {reviewed}")]
    ApprovalMismatch { approved: String, reviewed: String },
    #[error("approval denied: {0}")]
    ApprovalDenied(String),
    #[error("approver failed: {0}")]
    ApproverFailed(String),
    #[error("approval belongs to a different quarantine")]
    WrongQuarantine,
    #[error("approved commit {approved} is not a tip of the latest import")]
    TipMismatch { approved: String },
    #[error("commit {commit} has tree {actual}, but the approval is for tree {approved}")]
    TreeMismatch {
        commit: String,
        approved: String,
        actual: String,
    },
    #[error("remote rejected the push as non-fast-forward")]
    NonFastForward,
    #[error("push failed: {detail}")]
    PushRejected { detail: String },
    #[error("push ran, but the remote ref could not be confirmed at the approved commit: {0}")]
    RemoteVerifyFailed(String),
    #[error("archive contains an unsafe path: {0}")]
    PathTraversal(String),
    #[error("archive entries collide on this filesystem: {0}")]
    SnapshotCollision(String),
    #[error("archive is malformed: {0}")]
    SnapshotFormat(&'static str),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// " (exit code 128)", or nothing when git was ended by a signal.
fn exit_note(code: Option<i32>) -> String {
    code.map(|code| format!(" (exit code {code})")).unwrap_or_default()
}

pub type Result<T> = std::result::Result<T, Error>;
