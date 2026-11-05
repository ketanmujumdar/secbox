//! Push one approved commit to one task branch.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::{fmt, fs};

use serde::Serialize;

use crate::approval::Approval;
use crate::display::escape_stderr;
use crate::error::{Error, Result};
use crate::git::{GitCmd, Protocol, NETWORK_TIMEOUT};
use crate::quarantine::Quarantine;
use crate::validate::{validate_remote_url, validate_task_name};

/// How a push authenticates. This never reads the user's credentials. See the
/// crate docs for the credential helper contract.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct PushAuth {
    credential_helper: Option<PathBuf>,
    allow_local_remote: bool,
}

impl fmt::Debug for PushAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PushAuth")
            .field("has_credential_helper", &self.credential_helper.is_some())
            .field("allow_local_remote", &self.allow_local_remote)
            .finish()
    }
}

impl PushAuth {
    /// No credentials. Enough for a public test remote and nothing else: git has
    /// no prompt and no helper, so an authenticated remote fails.
    pub fn none() -> Self {
        Self::default()
    }

    /// Use one trusted helper for this push. The path must be absolute, made of
    /// shell-safe characters since git runs helpers through `sh`, a regular file
    /// and not a symlink, executable, and not writable by group or others.
    pub fn with_credential_helper(path: &Path) -> Result<Self> {
        let bad = Error::InvalidCredentialHelper;
        let text = path.to_str().ok_or(bad("path is not UTF-8"))?;
        if !path.is_absolute() {
            return Err(bad("path is not absolute"));
        }
        let safe = text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'));
        if !safe {
            return Err(bad("path holds characters unsafe for a shell"));
        }
        let meta = fs::symlink_metadata(path).map_err(|_| bad("path cannot be read"))?;
        if !meta.is_file() {
            return Err(bad("path is not a regular file"));
        }
        let mode = meta.permissions().mode();
        if mode & 0o111 == 0 {
            return Err(bad("file is not executable"));
        }
        if mode & 0o022 != 0 {
            return Err(bad("file is writable by group or others"));
        }
        Ok(Self {
            credential_helper: Some(path.to_path_buf()),
            allow_local_remote: false,
        })
    }

    /// Allow an absolute local path as a remote. Off by default. Only for a local
    /// bare repository: its own hooks and config will apply on its side.
    pub fn allowing_local_remote(mut self) -> Self {
        self.allow_local_remote = true;
        self
    }
}

/// What a successful push did. The remote ref was read back and confirmed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PushReceipt {
    pub commit: String,
    pub tree: String,
    pub remote_ref: String,
}

impl Quarantine {
    /// Push the approved commit to `refs/heads/<branch prefix><task>`.
    ///
    /// The refspec is always `<full sha>:<that ref>`, with no `+`, no force, no
    /// tags and no mirror, so a non-fast-forward fails. Before pushing, this
    /// re-checks that the approval belongs to this quarantine, that the commit is
    /// reachable from an imported ref, that its tree is still the approved tree,
    /// and that it is still a tip of the latest import. A newer import with a
    /// different tip means the proposal changed, so the old approval no longer
    /// applies. After pushing, the remote ref is read back.
    pub fn push(
        &self,
        approval: Approval,
        remote_url: &str,
        task: &str,
        auth: &PushAuth,
    ) -> Result<PushReceipt> {
        validate_task_name(task)?;
        validate_remote_url(remote_url, auth.allow_local_remote)?;

        if approval.quarantine != self.path() {
            return Err(Error::WrongQuarantine);
        }
        self.require_reachable(&approval.commit, &["refs/quarantine/"])?;
        let tree = self.tree_of(&approval.commit)?;
        if tree != approval.tree {
            return Err(Error::TreeMismatch {
                commit: approval.commit.clone(),
                approved: approval.tree.clone(),
                actual: tree,
            });
        }
        if !self.latest_import_tips()?.contains(&approval.commit) {
            return Err(Error::TipMismatch {
                approved: approval.commit.clone(),
            });
        }

        let remote_ref = format!("refs/heads/{}{task}", brand::BRANCH_PREFIX);
        let refspec = format!("{}:{remote_ref}", approval.commit);
        let out = self
            .transport_cmd("push", remote_url, auth)
            .args([
                "push",
                "--porcelain",
                "--atomic",
                "--no-follow-tags",
                "--no-signed",
                "--no-verify",
                "--no-recurse-submodules",
                "--",
                remote_url,
                refspec.as_str(),
            ])
            .timeout(NETWORK_TIMEOUT)
            .run()?;
        if !out.success() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            if stdout.contains("non-fast-forward") || stdout.contains("fetch first") {
                return Err(Error::NonFastForward);
            }
            let mut combined = out.stdout.clone();
            combined.extend_from_slice(&out.stderr);
            return Err(Error::PushRejected {
                detail: escape_stderr(&combined),
            });
        }

        let listed = self
            .transport_cmd("ls-remote", remote_url, auth)
            .args(["ls-remote", "--refs", "--", remote_url, remote_ref.as_str()])
            .timeout(NETWORK_TIMEOUT)
            .run()?;
        if !listed.success() {
            return Err(Error::RemoteVerifyFailed(escape_stderr(&listed.stderr)));
        }
        let expected = format!("{}\t{remote_ref}", approval.commit);
        if listed.stdout_trimmed() != expected {
            return Err(Error::RemoteVerifyFailed(
                "remote ref is not at the approved commit".to_string(),
            ));
        }

        Ok(PushReceipt {
            commit: approval.commit,
            tree: approval.tree,
            remote_ref,
        })
    }

    fn transport_cmd(&self, op: &'static str, remote_url: &str, auth: &PushAuth) -> GitCmd {
        let protocol = if remote_url.starts_with('/') {
            Protocol::File
        } else {
            Protocol::Https
        };
        let mut cmd = self.git(op).protocol(protocol);
        if let Some(helper) = &auth.credential_helper {
            // The hardened config already reset the helper list to empty.
            cmd = cmd
                .config(format!("credential.helper={}", helper.display()))
                .config("credential.useHttpPath=true");
        }
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn write_helper(path: &Path, mode: u32) {
        fs::write(path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn credential_helper_must_be_a_safe_private_executable() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let good = root.join("helper");
        write_helper(&good, 0o700);
        assert!(PushAuth::with_credential_helper(&good).is_ok());

        let writable = root.join("writable");
        write_helper(&writable, 0o777);
        let plain = root.join("plain");
        write_helper(&plain, 0o600);
        let link = root.join("link");
        symlink(&good, &link).unwrap();
        let shelly = root.join("a;touch x");
        write_helper(&shelly, 0o700);
        for bad in [
            writable.as_path(),
            plain.as_path(),
            link.as_path(),
            shelly.as_path(),
            root.as_path(),
            Path::new("relative"),
            Path::new("/nonexistent/helper"),
        ] {
            assert!(
                PushAuth::with_credential_helper(bad).is_err(),
                "{}",
                bad.display()
            );
        }
    }

    #[test]
    fn helper_is_configured_after_the_reset_and_only_when_given() {
        let dir = tempfile::tempdir().unwrap();
        let quarantine = Quarantine::create(&dir.path().join("q.git")).unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let helper = root.join("helper");
        write_helper(&helper, 0o700);
        let auth = PushAuth::with_credential_helper(&helper).unwrap();
        let out = quarantine
            .transport_cmd("config", "https://example.invalid/r", &auth)
            .args(["config", "--get-all", "credential.helper"])
            .ok()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let values: Vec<&str> = text.lines().collect();
        assert_eq!(values, ["", helper.to_str().unwrap()]);

        let out = quarantine
            .transport_cmd("config", "https://example.invalid/r", &PushAuth::none())
            .args(["config", "--get-all", "credential.helper"])
            .ok()
            .unwrap();
        assert_eq!(out.stdout_trimmed(), "");
    }

    #[test]
    fn debug_output_hides_the_helper_path() {
        let auth = PushAuth {
            credential_helper: Some(PathBuf::from("/secret/place/helper")),
            allow_local_remote: false,
        };
        assert!(!format!("{auth:?}").contains("secret"));
    }
}
