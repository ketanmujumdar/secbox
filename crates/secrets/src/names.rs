use std::fmt;

use serde::{Deserialize, Serialize};

/// Validation failure for a name. Holds no input text, because a mistyped name
/// can be a pasted value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("secret name must match [A-Za-z_][A-Za-z0-9_]* and be at most 256 characters")]
    Secret,
    #[error("service name must match [A-Za-z0-9][A-Za-z0-9._-]* and be at most 128 characters")]
    Service,
    #[error("project id must match [A-Za-z0-9][A-Za-z0-9._-]* and be at most 128 characters")]
    Project,
    #[error(
        "bound host must be one exact lowercase DNS name, with no wildcard, IP or trailing dot"
    )]
    BoundHost,
}

macro_rules! string_newtype {
    ($(#[$meta:meta])* $name:ident, $check:ident, $err:expr) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, NameError> {
                let value = value.into();
                if $check(&value) {
                    Ok(Self(value))
                } else {
                    Err($err)
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = NameError;
            fn try_from(value: String) -> Result<Self, NameError> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> String {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{:?}", self.0)
            }
        }
    };
}

string_newtype!(
    /// An environment-variable style secret name.
    SecretName,
    is_secret_name,
    NameError::Secret
);
string_newtype!(
    /// A Compose-style service name.
    ServiceName,
    is_dotted_name,
    NameError::Service
);
string_newtype!(
    /// A project id, used in the keychain account. It cannot contain `/`, so
    /// `project/name` accounts are unambiguous across projects.
    ProjectId,
    is_project_id,
    NameError::Project
);
string_newtype!(
    /// The single exact host a proxy secret may be sent to.
    BoundHost,
    is_bound_host,
    NameError::BoundHost
);

pub(crate) fn is_secret_name(s: &str) -> bool {
    let mut chars = s.chars();
    s.len() <= 256
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_dotted_name(s: &str) -> bool {
    let mut chars = s.chars();
    s.len() <= 128
        && chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn is_project_id(s: &str) -> bool {
    is_dotted_name(s)
}

fn is_bound_host(s: &str) -> bool {
    if s.is_empty() || s.len() > 253 {
        return false;
    }
    let labels: Vec<&str> = s.split('.').collect();
    let label_ok = |l: &&str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    // Require a final label that starts with a letter and is not `0x...`, so
    // numeric spellings of an IP such as `127.1` or `0x7f.1` cannot be bound.
    labels.len() >= 2
        && labels.iter().all(label_ok)
        && labels
            .last()
            .is_some_and(|l| l.starts_with(|c: char| c.is_ascii_lowercase()))
}
