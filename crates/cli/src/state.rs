//! The host's state folder. The code lives in its own crate because the daemon
//! needs the same view of it, and is re-exported here so that callers in this
//! crate keep one name for it.

pub use hoststate::*;
