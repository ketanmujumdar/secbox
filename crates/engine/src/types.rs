use std::fmt;
use std::time::Duration;

use crate::error::{EngineError, Result};

const MAX_NAME_BYTES: usize = 63;

/// A validated sandbox name: lowercase letters, digits and inner hyphens, at most
/// 63 bytes. That cannot be read as a CLI option or a path, and it is stricter
/// than the CLI's own limit of 128 bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SandboxName(String);

impl SandboxName {
    pub fn new(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        let invalid = |reason: &str| EngineError::InvalidInput {
            field: "sandbox name",
            reason: reason.to_owned(),
        };
        if name.is_empty() {
            return Err(invalid("must not be empty"));
        }
        if name.len() > MAX_NAME_BYTES {
            return Err(invalid("must be at most 63 bytes"));
        }
        let allowed = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
        if !name.chars().all(allowed) {
            return Err(invalid("may only use a-z, 0-9 and hyphens"));
        }
        if name.starts_with('-') || name.ends_with('-') {
            return Err(invalid("must not start or end with a hyphen"));
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SandboxName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An absolute path inside the guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestPath(String);

impl GuestPath {
    pub fn new(path: impl Into<String>) -> Result<Self> {
        let path = path.into();
        let invalid = |reason: &str| EngineError::InvalidInput {
            field: "guest path",
            reason: reason.to_owned(),
        };
        if !path.starts_with('/') {
            return Err(invalid("must be absolute"));
        }
        if path.contains('\0') || path.contains('\n') {
            return Err(invalid("must not contain NUL or newline"));
        }
        if path.split('/').any(|part| part == "..") {
            return Err(invalid("must not contain .."));
        }
        Ok(Self(path))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Per-sandbox limits, requested at creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resources {
    pub memory_mib: u32,
    pub cpus: u32,
    pub disk_gib: u32,
}

impl Resources {
    pub fn new(memory_mib: u32, cpus: u32, disk_gib: u32) -> Result<Self> {
        for (field, value) in [("memory", memory_mib), ("cpus", cpus), ("disk", disk_gib)] {
            if value == 0 {
                return Err(EngineError::InvalidInput {
                    field,
                    reason: "must be greater than zero".to_owned(),
                });
            }
        }
        Ok(Self {
            memory_mib,
            cpus,
            disk_gib,
        })
    }
}

/// How HTTPS to an allowed hostname is checked. The CLI's strict default denies
/// HTTPS allowed only by hostname, so one of these is always required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsMode {
    /// Decides from the TLS server name, without decrypting. Nested containers
    /// and build steps need no extra CA. Secrets are not available.
    ServerNameOnly,
    /// The gateway terminates TLS. Needed for secrets. Nested containers and build
    /// steps do not trust its CA by default, so their HTTPS fails until it is added.
    Intercept,
}

/// Outbound network policy. Never includes the host or private networks.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NetworkPolicy {
    /// No outbound traffic.
    #[default]
    DenyAll,
    /// The public internet, the CLI's own default.
    PublicInternet,
    /// Only these hostnames. DNS is filtered to the same names.
    AllowHosts { hosts: Vec<String>, tls: TlsMode },
}

/// A secret the guest holds only as a placeholder. The gateway swaps in the real
/// value on requests to `hosts`. The value itself is given to the engine separately,
/// never in a spec, so it cannot end up in a log of the spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRule {
    pub env: String,
    pub hosts: Vec<String>,
}

/// A published port. There is deliberately no bind address field: every port is
/// forwarded on 127.0.0.1 and keeps the same number on both sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostPort(pub u16);

/// Everything needed to create a sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateSpec {
    pub name: SandboxName,
    pub image: String,
    pub resources: Resources,
    pub network: NetworkPolicy,
    pub secrets: Vec<SecretRule>,
    pub ports: Vec<HostPort>,
}

/// Sandbox state as far as the engine can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxStatus {
    NotFound,
    Stopped,
    Running,
    /// A state the adapter has not seen, reported as the CLI gave it.
    Other(String),
}

/// A command to run in the guest. Arguments are passed as they are, with no shell
/// on the host side. `env` values appear in the host's process list while the
/// command runs, so secrets must never go there: use [`SecretRule`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecRequest {
    pub command: Vec<String>,
    pub timeout: Option<Duration>,
    pub env: Vec<(String, String)>,
    pub workdir: Option<GuestPath>,
}

impl ExecRequest {
    pub fn new<I, S>(command: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            command: command.into_iter().map(Into::into).collect(),
            timeout: None,
            env: Vec::new(),
            workdir: None,
        }
    }
}

/// The result of a command that ran. A non-zero `code` is not an engine error.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExecOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Which log entries to return.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LogOptions {
    pub tail: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_names() {
        for name in ["a", "demo-shop", "p1-2", &"a".repeat(63)] {
            assert!(SandboxName::new(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn rejects_unsafe_names() {
        for name in [
            "",
            "-rf",
            "--force",
            "trail-",
            "UPPER",
            "a b",
            "a/b",
            "../x",
            "a:b",
            "a\nb",
            "a\0b",
            "a;b",
            "$(x)",
            "é",
            &"a".repeat(64),
        ] {
            assert!(
                matches!(
                    SandboxName::new(name),
                    Err(EngineError::InvalidInput { .. })
                ),
                "{name:?}"
            );
        }
    }

    #[test]
    fn guest_path_rules() {
        assert!(GuestPath::new("/work/app").is_ok());
        for path in ["work", "", "/a/../b", "/a\0", "/a\nb"] {
            assert!(GuestPath::new(path).is_err(), "{path:?}");
        }
    }

    #[test]
    fn resources_reject_zero() {
        assert!(Resources::new(1, 1, 1).is_ok());
        assert!(Resources::new(0, 1, 1).is_err());
        assert!(Resources::new(1, 0, 1).is_err());
        assert!(Resources::new(1, 1, 0).is_err());
    }

    #[test]
    fn default_network_denies() {
        assert_eq!(NetworkPolicy::default(), NetworkPolicy::DenyAll);
    }
}
