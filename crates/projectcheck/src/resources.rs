//! The `resources` block of the project config file named by
//! [`brand::CONFIG_FILE`].
//!
//! The repo's values are requests. [`resolve`] lowers them to the user's
//! maximum and refuses to start if the host would be left with too little.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use serde::Serialize;
use serde_norway::Value;

use crate::safe_fs::{read_inside, resolve_inside, Entry};
use crate::{clean, yaml, Error};

/// The largest project config file that will be parsed.
pub const MAX_CONFIG_BYTES: u64 = 64 * 1024;

const GIB: u64 = 1024 * 1024 * 1024;
const MAX_SIZE_TEXT: usize = 32;
const MAX_MILLICPUS: u32 = 4096 * 1000;

/// Memory used when the repo makes no request, if the user's maximum allows it.
pub const DEFAULT_MEMORY: ByteSize = ByteSize(4 * GIB);
/// CPUs used when the repo makes no request, if the user's maximum allows it.
pub const DEFAULT_CPUS: MilliCpus = MilliCpus(2000);
/// Disk used when the repo makes no request, if the user's maximum allows it.
pub const DEFAULT_DISK: ByteSize = ByteSize(20 * GIB);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResourceError {
    #[error("{field}: {reason}")]
    InvalidValue { field: String, reason: String },
    #[error("unknown key `{key}` in {place}")]
    UnknownKey { key: String, place: String },
    #[error("{place} must be a mapping")]
    NotAMapping { place: String },
    #[error(
        "not enough {resource}: {needed} needed, {available} available, and {reserve} must stay free for the host"
    )]
    HostReserve {
        resource: &'static str,
        needed: String,
        available: String,
        reserve: String,
    },
}

/// Failure to load the config file.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error(transparent)]
    File(#[from] Error),
    #[error(transparent)]
    Resource(#[from] ResourceError),
}

/// A size in bytes, never zero. `6G` means 6 GiB, as in Docker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ByteSize(u64);

impl ByteSize {
    /// Returns `None` for zero.
    pub const fn new(bytes: u64) -> Option<Self> {
        if bytes == 0 {
            None
        } else {
            Some(Self(bytes))
        }
    }

    pub const fn bytes(self) -> u64 {
        self.0
    }
}

impl FromStr for ByteSize {
    type Err = String;

    /// Accepts a number with a required unit: `512M`, `6G`, `1.5T`, `6GiB`.
    fn from_str(text: &str) -> Result<Self, String> {
        if text.len() > MAX_SIZE_TEXT {
            return Err("the size is too long".to_string());
        }
        let split = text
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .ok_or("the size has no unit, such as 512M or 6G")?;
        let (number, unit) = text.split_at(split);
        let multiplier: u128 = match unit.to_ascii_lowercase().as_str() {
            "b" => 1,
            "k" | "kb" | "ki" | "kib" => 1 << 10,
            "m" | "mb" | "mi" | "mib" => 1 << 20,
            "g" | "gb" | "gi" | "gib" => 1 << 30,
            "t" | "tb" | "ti" | "tib" => 1 << 40,
            _ => return Err("the size has an unknown unit".to_string()),
        };
        let (whole, fraction) = parse_decimal(number).ok_or("the size is not a valid number")?;
        let (fraction, scale) = fraction;
        let fraction_bytes = fraction * multiplier;
        if !fraction_bytes.is_multiple_of(scale) {
            return Err("the size is not a whole number of bytes".to_string());
        }
        let total = whole
            .checked_mul(multiplier)
            .and_then(|bytes| bytes.checked_add(fraction_bytes / scale))
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or("the size is too large")?;
        ByteSize::new(total).ok_or_else(|| "the size must be above zero".to_string())
    }
}

impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write_bytes(f, self.0)
    }
}

fn write_bytes(f: &mut fmt::Formatter, bytes: u64) -> fmt::Result {
    for (unit, size) in [
        ("T", 1u64 << 40),
        ("G", 1 << 30),
        ("M", 1 << 20),
        ("K", 1 << 10),
    ] {
        if bytes >= size && bytes.is_multiple_of(size) {
            return write!(f, "{}{unit}", bytes / size);
        }
    }
    write!(f, "{bytes}B")
}

/// Parses ASCII `12` or `12.5` into the whole part, plus the fraction as
/// (numerator, denominator). No sign, exponent or surrounding space.
fn parse_decimal(text: &str) -> Option<(u128, (u128, u128))> {
    let (whole, fraction) = match text.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (text, None),
    };
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let whole = whole.parse::<u128>().ok()?;
    let Some(fraction) = fraction else {
        return Some((whole, (0, 1)));
    };
    if fraction.is_empty() || fraction.len() > 9 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let scale = 10u128.pow(u32::try_from(fraction.len()).ok()?);
    Some((whole, (fraction.parse().ok()?, scale)))
}

/// A CPU amount in thousandths of a CPU, never zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct MilliCpus(u32);

impl MilliCpus {
    /// Returns `None` for zero or an amount above 4096 CPUs.
    pub const fn new(millis: u32) -> Option<Self> {
        if millis == 0 || millis > MAX_MILLICPUS {
            None
        } else {
            Some(Self(millis))
        }
    }

    pub const fn millis(self) -> u32 {
        self.0
    }
}

impl FromStr for MilliCpus {
    type Err = String;

    /// Accepts `4`, `0.5` or `1.25`, with at most three decimal places.
    fn from_str(text: &str) -> Result<Self, String> {
        if text.len() > MAX_SIZE_TEXT {
            return Err("the CPU count is too long".to_string());
        }
        let (whole, (fraction, scale)) =
            parse_decimal(text).ok_or("the CPU count is not a valid number")?;
        if scale > 1000 {
            return Err("the CPU count has more than three decimal places".to_string());
        }
        let millis = whole
            .checked_mul(1000)
            .and_then(|m| m.checked_add(fraction * 1000 / scale))
            .ok_or("the CPU count is too large")?;
        if millis == 0 {
            return Err("the CPU count must be above zero".to_string());
        }
        u32::try_from(millis)
            .ok()
            .and_then(MilliCpus::new)
            .ok_or_else(|| "the CPU count is too large".to_string())
    }
}

impl fmt::Display for MilliCpus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write_millis(f, self.0)
    }
}

fn write_millis(f: &mut fmt::Formatter, millis: u32) -> fmt::Result {
    if millis.is_multiple_of(1000) {
        write!(f, "{}", millis / 1000)
    } else {
        write!(f, "{}.{:03}", millis / 1000, millis % 1000)
    }
}

/// What the repo asks for. Absent values were not requested.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ResourceRequest {
    pub memory: Option<ByteSize>,
    pub cpus: Option<MilliCpus>,
    pub disk: Option<ByteSize>,
    pub build_memory: Option<ByteSize>,
}

/// The user's maximums. The repo cannot raise these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct UserLimits {
    pub max_memory: ByteSize,
    pub max_cpus: MilliCpus,
    pub max_disk: ByteSize,
    /// The ceiling during builds. Absent means `max_memory`.
    pub max_build_memory: Option<ByteSize>,
}

/// What the host has now, and what must stay free. `available_*` should
/// already exclude what other running projects hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HostResources {
    pub available_memory: u64,
    pub memory_reserve: u64,
    pub available_millicpus: u32,
    pub cpu_reserve_millis: u32,
    pub available_disk: u64,
    pub disk_reserve: u64,
}

/// A value the user's maximum lowered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Clamped {
    Memory,
    Cpus,
    Disk,
    BuildMemory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedResources {
    pub memory: ByteSize,
    pub cpus: MilliCpus,
    pub disk: ByteSize,
    /// The limit during builds. It equals `memory` unless requested otherwise.
    pub build_memory: ByteSize,
    /// Requests lowered to the user's maximum, to show the user.
    pub clamped: Vec<Clamped>,
}

/// Parses the text of the config file. An empty file requests nothing. Unknown
/// keys at any level are errors.
pub fn parse_config(text: &str) -> Result<ResourceRequest, ConfigError> {
    if text.len() as u64 > MAX_CONFIG_BYTES {
        return Err(Error::InputTooLarge {
            limit: MAX_CONFIG_BYTES,
        }
        .into());
    }
    let doc = yaml::load(text)?;
    let place = brand::CONFIG_FILE;
    if yaml::has_tag(&doc) {
        return Err(ResourceError::InvalidValue {
            field: place.to_string(),
            reason: "YAML tags are not allowed".to_string(),
        }
        .into());
    }
    let root = match &doc {
        Value::Null => return Ok(ResourceRequest::default()),
        Value::Mapping(map) => map,
        _ => {
            return Err(ResourceError::NotAMapping {
                place: place.to_string(),
            }
            .into())
        }
    };
    let mut request = ResourceRequest::default();
    for (key, value) in root {
        match key.as_str() {
            Some("resources") => request = parse_resources(value)?,
            _ => {
                return Err(ResourceError::UnknownKey {
                    key: key_label(key),
                    place: place.to_string(),
                }
                .into())
            }
        }
    }
    Ok(request)
}

/// Reads and parses the config file at the project root. A missing file
/// requests nothing. A link out of the project is an error.
pub fn load(root: &Path) -> Result<ResourceRequest, ConfigError> {
    let rel = Path::new(brand::CONFIG_FILE);
    if resolve_inside(root, rel)? == Entry::Missing {
        return Ok(ResourceRequest::default());
    }
    parse_config(&read_inside(root, rel, MAX_CONFIG_BYTES)?)
}

fn parse_resources(value: &Value) -> Result<ResourceRequest, ResourceError> {
    let map = value.as_mapping().ok_or(ResourceError::NotAMapping {
        place: "resources".to_string(),
    })?;
    let mut request = ResourceRequest::default();
    for (key, item) in map {
        match key.as_str() {
            Some("memory") => request.memory = Some(parse_size("resources.memory", item)?),
            Some("cpus") => request.cpus = Some(parse_cpus(item)?),
            Some("disk") => request.disk = Some(parse_size("resources.disk", item)?),
            Some("build") => request.build_memory = parse_build(item)?,
            _ => {
                return Err(ResourceError::UnknownKey {
                    key: key_label(key),
                    place: "resources".to_string(),
                })
            }
        }
    }
    Ok(request)
}

fn parse_build(value: &Value) -> Result<Option<ByteSize>, ResourceError> {
    let map = value.as_mapping().ok_or(ResourceError::NotAMapping {
        place: "resources.build".to_string(),
    })?;
    let mut memory = None;
    for (key, item) in map {
        match key.as_str() {
            Some("memory") => memory = Some(parse_size("resources.build.memory", item)?),
            _ => {
                return Err(ResourceError::UnknownKey {
                    key: key_label(key),
                    place: "resources.build".to_string(),
                })
            }
        }
    }
    Ok(memory)
}

fn parse_size(field: &str, value: &Value) -> Result<ByteSize, ResourceError> {
    let invalid = |reason: String| ResourceError::InvalidValue {
        field: field.to_string(),
        reason,
    };
    let text = value
        .as_str()
        .ok_or_else(|| invalid("expected a size with a unit, such as 512M or 6G".to_string()))?;
    text.parse().map_err(invalid)
}

fn parse_cpus(value: &Value) -> Result<MilliCpus, ResourceError> {
    let invalid = |reason: String| ResourceError::InvalidValue {
        field: "resources.cpus".to_string(),
        reason,
    };
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        _ => return Err(invalid("expected a number of CPUs".to_string())),
    };
    text.parse().map_err(invalid)
}

fn key_label(key: &Value) -> String {
    key.as_str()
        .map_or_else(|| "<not a string>".to_string(), clean)
}

/// Fits a request to the user's limits and the host.
///
/// A value above the user's maximum is lowered, and listed in `clamped`. A
/// value not requested uses the matching default, up to the maximum. If the
/// result would leave less than the host reserve, this returns an error rather
/// than lowering the value further. Memory is checked at the higher of the run
/// and build limits, since a build holds that much on the same VM.
pub fn resolve(
    request: &ResourceRequest,
    limits: &UserLimits,
    host: &HostResources,
) -> Result<ResolvedResources, ResourceError> {
    let mut clamped = Vec::new();
    let memory = fit(
        request.memory,
        DEFAULT_MEMORY,
        limits.max_memory,
        Clamped::Memory,
        &mut clamped,
    );
    let cpus = fit(
        request.cpus,
        DEFAULT_CPUS,
        limits.max_cpus,
        Clamped::Cpus,
        &mut clamped,
    );
    let disk = fit(
        request.disk,
        DEFAULT_DISK,
        limits.max_disk,
        Clamped::Disk,
        &mut clamped,
    );
    let build_memory = fit(
        request.build_memory,
        memory,
        limits.max_build_memory.unwrap_or(limits.max_memory),
        Clamped::BuildMemory,
        &mut clamped,
    );

    let peak = memory.max(build_memory);
    if peak.bytes() > host.available_memory.saturating_sub(host.memory_reserve) {
        return Err(ResourceError::HostReserve {
            resource: "memory",
            needed: peak.to_string(),
            available: show_bytes(host.available_memory),
            reserve: show_bytes(host.memory_reserve),
        });
    }
    if cpus.millis()
        > host
            .available_millicpus
            .saturating_sub(host.cpu_reserve_millis)
    {
        return Err(ResourceError::HostReserve {
            resource: "CPU",
            needed: cpus.to_string(),
            available: show_millis(host.available_millicpus),
            reserve: show_millis(host.cpu_reserve_millis),
        });
    }
    if disk.bytes() > host.available_disk.saturating_sub(host.disk_reserve) {
        return Err(ResourceError::HostReserve {
            resource: "disk",
            needed: disk.to_string(),
            available: show_bytes(host.available_disk),
            reserve: show_bytes(host.disk_reserve),
        });
    }
    Ok(ResolvedResources {
        memory,
        cpus,
        disk,
        build_memory,
        clamped,
    })
}

fn fit<T: Copy + Ord>(
    requested: Option<T>,
    default: T,
    maximum: T,
    label: Clamped,
    clamped: &mut Vec<Clamped>,
) -> T {
    match requested {
        Some(value) if value > maximum => {
            clamped.push(label);
            maximum
        }
        Some(value) => value,
        None => default.min(maximum),
    }
}

struct ShowBytes(u64);

impl fmt::Display for ShowBytes {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write_bytes(f, self.0)
    }
}

fn show_bytes(bytes: u64) -> String {
    ShowBytes(bytes).to_string()
}

struct ShowMillis(u32);

impl fmt::Display for ShowMillis {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write_millis(f, self.0)
    }
}

fn show_millis(millis: u32) -> String {
    ShowMillis(millis).to_string()
}
