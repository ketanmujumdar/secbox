//! The host's record of what happened.
//!
//! It is written by the daemon, on the host, so a compromised sandbox cannot
//! edit or hide an entry. Every field is either a fixed word from this file or
//! a name: never a secret value, never a file's contents, never a line a
//! sandbox chose. Anything that did come from outside has already been through
//! [`proto::text::escape`] and is cut short, so one entry cannot forge another.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const AUDIT_FILE: &str = "daemon-audit.jsonl";
const MAX_DETAIL_CHARS: usize = 200;

/// What happened. Each is a fixed word, so the log cannot be filled with text
/// chosen elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    DaemonStarted,
    PlanBuilt,
    SandboxCreated,
    SandboxStarted,
    SandboxStopped,
    SandboxDestroyed,
    /// A project and its records were removed from this machine.
    ProjectRemoved,
    /// A person let a sandbox reach one more hostname.
    HostApproved,
    /// A port was forwarded to `127.0.0.1` after a person approved it.
    PortForwarded,
    /// A port the project asked for was not forwarded.
    PortRefused,
    /// A secret was delivered to a service. The name only.
    SecretDelivered,
    /// A secret was not delivered, and why.
    SecretRefused,
    SnapshotTaken,
    /// A project was fetched from a Git remote, over SSH, on the host.
    RemoteFetched,
    /// A pull ran through the relay.
    GitPulled,
    /// A push changed this branch, from one commit to another.
    GitPushed,
    /// A push was stopped before anything was sent, and why.
    GitPushRefused,
    /// A file was deliberately left out of a snapshot.
    SnapshotExcluded,
    BuildStarted,
    BuildFinished,
    RequestRefused,
    /// A client was told how to open a terminal in a running sandbox. What
    /// happens in that terminal is not recorded.
    TerminalOpened,
    /// The host made a commit of the snapshot to compare later changes with.
    BaselineMade,
    /// No such commit could be made, so changes in this sandbox cannot be
    /// compared. The sandbox starts anyway.
    BaselineMissing,
    /// A bundle from a sandbox was reviewed on the host. Counts only.
    ChangesReviewed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub at_unix: u64,
    pub action: Action,
    /// The project this concerns, by its stable identity.
    pub project: Option<String>,
    /// A name: a secret's name, a service's name, a host, a port. Never a value.
    pub subject: Option<String>,
    /// A short reason, already safe to print.
    pub detail: Option<String>,
}

/// Appends entries to one file, owner-only.
#[derive(Debug, Clone)]
pub struct Log {
    path: PathBuf,
}

impl Log {
    pub fn in_state_dir(state: &Path) -> Self {
        Self {
            path: state.join(AUDIT_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes one entry. A failure here is reported to the caller rather than
    /// ignored: an action that cannot be recorded should not look like one
    /// that was.
    pub fn write(&self, entry: &Entry) -> Result<(), String> {
        if std::fs::symlink_metadata(&self.path).is_ok_and(|m| !m.is_file()) {
            return Err("the audit log is not a regular file".to_owned());
        }
        let mut line =
            serde_json::to_vec(entry).map_err(|_| "cannot encode an audit entry".to_owned())?;
        line.push(b'\n');
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)
            .and_then(|mut file| file.write_all(&line))
            .map_err(|e| format!("cannot write the audit log: {}", e.kind()))
    }
}

/// Builds an entry, cutting and escaping anything that came from outside.
pub fn entry(at_unix: u64, action: Action) -> Entry {
    Entry {
        at_unix,
        action,
        project: None,
        subject: None,
        detail: None,
    }
}

impl Entry {
    #[must_use]
    pub fn project(mut self, id: &str) -> Self {
        self.project = Some(proto::text::escape_short(id, MAX_DETAIL_CHARS));
        self
    }

    #[must_use]
    pub fn subject(mut self, subject: impl AsRef<str>) -> Self {
        self.subject = Some(proto::text::escape_short(
            subject.as_ref(),
            MAX_DETAIL_CHARS,
        ));
        self
    }

    #[must_use]
    pub fn detail(mut self, detail: impl AsRef<str>) -> Self {
        self.detail = Some(proto::text::escape_short(detail.as_ref(), MAX_DETAIL_CHARS));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn entries_append_one_line_each() {
        let dir = temp_dir();
        let log = Log::in_state_dir(dir.path());
        log.write(&entry(1, Action::SandboxCreated).project("p-1"))
            .unwrap();
        log.write(&entry(2, Action::SandboxStopped).project("p-1"))
            .unwrap();
        let text = std::fs::read_to_string(log.path()).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("sandbox_created"));
        assert!(text.contains("sandbox_stopped"));
    }

    #[test]
    fn the_log_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir();
        let log = Log::in_state_dir(dir.path());
        log.write(&entry(1, Action::PlanBuilt)).unwrap();
        let mode = std::fs::metadata(log.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "mode was {mode:o}");
    }

    /// A name coming from a project file could otherwise carry a newline and
    /// write a second entry of its own choosing.
    #[test]
    fn a_hostile_name_cannot_forge_an_entry() {
        let dir = temp_dir();
        let log = Log::in_state_dir(dir.path());
        log.write(
            &entry(1, Action::SecretDelivered)
                .subject("api\n{\"action\":\"sandbox_destroyed\"}")
                .detail("x\u{1b}[31m"),
        )
        .unwrap();
        let text = std::fs::read_to_string(log.path()).unwrap();
        // One line, and the text it tried to smuggle stayed a value inside
        // that line rather than becoming an entry of its own.
        assert_eq!(text.lines().count(), 1);
        let parsed: Entry = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(parsed.action, Action::SecretDelivered);
        assert!(!text.contains('\u{1b}'));
        assert!(parsed.subject.unwrap().contains("\\u{a}"));
    }

    #[test]
    fn a_long_name_is_cut() {
        let long = "n".repeat(10_000);
        let e = entry(1, Action::SecretDelivered).subject(&long);
        assert!(e.subject.unwrap().chars().count() < 250);
    }

    #[test]
    fn a_log_that_is_not_a_regular_file_is_refused() {
        let dir = temp_dir();
        std::fs::create_dir(dir.path().join(AUDIT_FILE)).unwrap();
        let log = Log::in_state_dir(dir.path());
        assert!(log.write(&entry(1, Action::PlanBuilt)).is_err());
    }
}
