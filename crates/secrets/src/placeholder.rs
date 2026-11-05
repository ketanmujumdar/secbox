use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;

use crate::assignment::{AssignmentSet, Delivery};
use crate::names::SecretName;

const RANDOM_BYTES: usize = 16;

/// A random stand-in for a proxy secret. It is safe to place in the VM and
/// safe to log, so it is an ordinary string type rather than a `SecretValue`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Placeholder(String);

impl Placeholder {
    /// Generates a new placeholder of the form `<product>-placeholder-<32 hex>`.
    pub fn generate() -> Result<Self, PlaceholderError> {
        let bytes = os_random()?;
        let mut hex = String::with_capacity(RANDOM_BYTES * 2);
        for byte in bytes {
            // Writing to a String cannot fail.
            let _ = write!(hex, "{byte:02x}");
        }
        Ok(Self(format!("{}-placeholder-{hex}", brand::PRODUCT_NAME)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PlaceholderError {
    #[error("the OS randomness source is unavailable")]
    RandomUnavailable,
}

/// Reads from the OS CSPRNG through `/dev/urandom`.
///
/// This avoids adding a dependency for 16 bytes, in line with the project's
/// few-dependencies rule. The supported hosts are macOS and Linux, where the
/// device is always present. Any failure, including the path not being a
/// character device, is an error, because there is no weaker fallback.
#[cfg(unix)]
fn os_random() -> Result<[u8; RANDOM_BYTES], PlaceholderError> {
    use std::fs::File;
    use std::io::Read;
    use std::os::unix::fs::FileTypeExt;

    let mut device = File::open("/dev/urandom").map_err(|_| PlaceholderError::RandomUnavailable)?;
    let is_char_device = device
        .metadata()
        .map(|m| m.file_type().is_char_device())
        .unwrap_or(false);
    if !is_char_device {
        return Err(PlaceholderError::RandomUnavailable);
    }
    let mut bytes = [0u8; RANDOM_BYTES];
    device
        .read_exact(&mut bytes)
        .map_err(|_| PlaceholderError::RandomUnavailable)?;
    Ok(bytes)
}

#[cfg(not(unix))]
fn os_random() -> Result<[u8; RANDOM_BYTES], PlaceholderError> {
    Err(PlaceholderError::RandomUnavailable)
}

/// The placeholders for one sandbox. Generate a fresh set for each sandbox, so
/// that a placeholder seen in one VM means nothing in another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPlaceholders {
    by_name: BTreeMap<SecretName, Placeholder>,
}

impl SandboxPlaceholders {
    /// Generates one placeholder for each proxy secret in `assignments`.
    pub fn generate(assignments: &AssignmentSet) -> Result<Self, PlaceholderError> {
        let mut by_name = BTreeMap::new();
        for assignment in assignments.iter() {
            if matches!(assignment.delivery(), Delivery::Proxy { .. }) {
                by_name.insert(assignment.name().clone(), Placeholder::generate()?);
            }
        }
        Ok(Self { by_name })
    }

    pub fn get(&self, name: &SecretName) -> Option<&Placeholder> {
        self.by_name.get(name)
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}
