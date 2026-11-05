//! Host-side broker between a compromised VM and a git remote.
//!
//! Code in the VM produces a git bundle. This crate takes that bundle in as
//! hostile data, shows it for review without executing anything from it, and
//! pushes only an exact approved commit to one task branch.
//!
//! ```text
//! Quarantine::create -> import_bundle -> review -> approve -> push
//! snapshot: trusted repo -> directory for the VM, without .env files
//! ```
//!
//! Every git process goes through one hardened helper (`git` module): fixed
//! argv with no shell, a scrubbed environment, an empty temporary `HOME`, hooks
//! and external helpers disabled by command-line config, and a timeout.
//!
//! # Credentials
//!
//! This crate never reads the user's credentials, and global and system git
//! config are not loaded, so no helper is picked up from them. To authenticate
//! a push, give [`PushAuth::with_credential_helper`] the absolute path of a
//! trusted executable. It becomes the only helper for that one push:
//!
//! ```text
//! git -c credential.helper= -c credential.helper=/abs/path/helper -c credential.useHttpPath=true push ...
//! ```
//!
//! The helper speaks the normal git credential protocol: it is run with `get`,
//! reads `protocol`, `host` and `path` on stdin, and prints `username=` and
//! `password=`. Git may also call it with `store` and `erase`. Git runs helpers
//! through `sh`, which is why the path is restricted to shell-safe characters.
//! The helper inherits the scrubbed environment, so it has to find its secret
//! itself, for example from the keychain. Credentials embedded in a remote URL
//! are rejected.
//!
//! # What this does not defend against
//!
//! Git parses hostile packs in the host process tree. Hooks and config are
//! neutralized, but a memory-safety bug in git's pack or object parsing is not.
//! The review is a heuristic aid for a person and does not prove a commit safe.

#![forbid(unsafe_code)]

#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod common;

#[cfg(test)]
mod internal_tests;

mod approval;
mod bundle;
mod detect;
mod display;
mod error;
mod git;
mod push;
mod quarantine;
mod review;
mod snapshot;
mod tar;
mod validate;

pub use approval::{approve, Approval, ApprovalRequest, Approver, ApproverError, Decision};
pub use detect::SecretRule;
pub use display::escape_for_display;
pub use error::Error;
pub use push::{PushAuth, PushReceipt};
pub use quarantine::{ImportedBundle, ImportedRef, Quarantine};
pub use review::{ChangeStatus, FileChange, Flag, FlagKind, Review, ReviewLimits};
pub use snapshot::{snapshot, ExcludedEntry, ExclusionReason, SnapshotReport};
pub use validate::{validate_remote_url, validate_task_name};
