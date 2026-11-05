use std::fmt;

/// Result type for engine operations.
pub type Result<T> = std::result::Result<T, EngineError>;

/// Errors from an engine. Text from commands has already been through the
/// engine's redactor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// A name, path or setting was rejected before anything ran.
    InvalidInput { field: &'static str, reason: String },
    /// This adapter has not verified the operation, so it will not attempt it.
    NotSupported {
        operation: &'static str,
        reason: &'static str,
    },
    /// The engine binary could not be found or is not a usable file.
    BinaryNotFound { path: String, reason: String },
    /// The engine's state directory is too long for its Unix sockets.
    HomePathTooLong {
        home: String,
        derived_bytes: usize,
        limit_bytes: usize,
    },
    /// The command could not be started.
    Spawn { operation: String, reason: String },
    /// The command ran and exited unsuccessfully.
    CommandFailed {
        operation: String,
        code: Option<i32>,
        stderr: String,
    },
    /// The command was killed after running past its time limit.
    Timeout { operation: String, seconds: u64 },
    /// The sandbox does not exist.
    NotFound { name: String },
    /// A sandbox with this name already exists.
    AlreadyExists { name: String },
    /// The sandbox is not in a state that allows this operation.
    InvalidState { name: String, reason: String },
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput { field, reason } => write!(f, "invalid {field}: {reason}"),
            Self::NotSupported { operation, reason } => {
                write!(f, "{operation} is not supported: {reason}")
            }
            Self::BinaryNotFound { path, reason } => {
                write!(f, "engine binary {path}: {reason}")
            }
            Self::HomePathTooLong {
                home,
                derived_bytes,
                limit_bytes,
            } => write!(
                f,
                "engine state directory {home} is too long: its socket path would be \
                 {derived_bytes} bytes, and it must be shorter than {limit_bytes}"
            ),
            Self::Spawn { operation, reason } => {
                write!(f, "could not start {operation}: {reason}")
            }
            Self::CommandFailed {
                operation,
                code,
                stderr,
            } => match code {
                Some(code) => write!(f, "{operation} exited with {code}: {stderr}"),
                None => write!(f, "{operation} was killed by a signal: {stderr}"),
            },
            Self::Timeout { operation, seconds } => {
                write!(
                    f,
                    "{operation} did not finish within {seconds}s and was killed"
                )
            }
            Self::NotFound { name } => write!(f, "sandbox {name} not found"),
            Self::AlreadyExists { name } => write!(f, "sandbox {name} already exists"),
            Self::InvalidState { name, reason } => write!(f, "sandbox {name}: {reason}"),
        }
    }
}

impl std::error::Error for EngineError {}
