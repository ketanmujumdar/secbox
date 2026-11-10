//! Secret handling for the host side: values, `.env` import, storage,
//! per-service assignment, placeholders and audit events.
//!
//! Values only ever appear inside [`SecretValue`], which has no `Clone`, no
//! `Serialize`, a redacted `Debug`/`Display`, and zeroes its buffer on drop.
//! Errors and audit events carry names and line numbers, never values.

mod assignment;
mod audit;
mod dotenv;
mod names;
mod placeholder;
mod store;
mod value;

pub use assignment::{
    Assignment, AssignmentError, AssignmentSet, DeliveredSecret, Delivery, ProxyBinding, ProxyEnv,
    ResolveError, ResolvedService,
};
pub use audit::{AuditAction, AuditEvent};
pub use dotenv::{
    is_env_file_name, is_template_file,
    parse_env, parse_env_bytes, parse_env_file, EnvEntry, EnvError, MAX_ENV_FILE_BYTES,
};
pub use names::{BoundHost, NameError, ProjectId, SecretName, ServiceName};
pub use placeholder::{Placeholder, PlaceholderError, SandboxPlaceholders};
pub use store::{keychain_accounts, split_account, KeyringStore, MemoryStore, SecretStore, StoreError};
pub use value::SecretValue;
