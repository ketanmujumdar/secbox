//! The VM engine boundary: an [`Engine`] trait, a [`FakeEngine`] for tests, and
//! [`MsbEngine`], an adapter for the microsandbox CLI.
//!
//! The adapter implements what was observed working in
//! `docs/spikes/0001-engine-compose.md`, on microsandbox v0.7.13 on macOS.

pub mod argv;
mod error;
mod fake;
mod msb;
mod process;
mod redact;
mod types;

pub use error::{EngineError, Result};
pub use fake::{FakeCall, FakeEngine};
pub use msb::{MsbEngine, MsbLocation, DERIVED_SOCKET_SUFFIX_BYTES, TESTED_VERSION};
pub use redact::{LiteralRedactor, NoRedaction, Redactor};
pub use types::{
    CreateSpec, EngineLocation, ExecOutput, ExecRequest, GuestPath, HostPort, LogOptions,
    NetworkPolicy, OutputSink, OutputStream, Resources, SandboxName, SandboxStatus, SecretRule,
    StreamEnd, TlsMode,
};

use std::path::Path;

/// Runs and manages one sandbox VM per project.
pub trait Engine: Send + Sync {
    /// Creates a sandbox with fixed resources. Host ports always bind to 127.0.0.1.
    fn create(&self, spec: &CreateSpec) -> Result<()>;
    /// Starts a stopped sandbox. Processes inside do not come back: restart them,
    /// after removing any stale `/var/run/docker.pid` for `dockerd`.
    fn start(&self, name: &SandboxName) -> Result<()>;
    /// Stops a running sandbox.
    fn stop(&self, name: &SandboxName) -> Result<()>;
    /// Removes a sandbox and everything in it, stopping it first if needed.
    fn destroy(&self, name: &SandboxName) -> Result<()>;
    /// Reports state. A sandbox that does not exist is [`SandboxStatus::NotFound`].
    fn status(&self, name: &SandboxName) -> Result<SandboxStatus>;
    /// Runs a command in a running sandbox, without a shell on the host side.
    fn exec(&self, name: &SandboxName, request: &ExecRequest) -> Result<ExecOutput>;
    /// Runs a command in a running sandbox and hands over its output as it
    /// arrives. It has no time limit unless the request sets one. A sandbox that
    /// is not running is refused, not booted.
    fn exec_stream(
        &self,
        name: &SandboxName,
        request: &ExecRequest,
        sink: &mut dyn OutputSink,
    ) -> Result<StreamEnd>;
    /// Starts a command that keeps running, and returns its job ID.
    fn exec_detached(&self, name: &SandboxName, request: &ExecRequest) -> Result<String>;
    /// Copies a host file or directory into a running sandbox.
    fn copy_in(&self, name: &SandboxName, host: &Path, guest: &GuestPath) -> Result<()>;
    /// Returns captured sandbox output.
    fn logs(&self, name: &SandboxName, options: &LogOptions) -> Result<String>;
    /// Where a client can run this engine on a person's own terminal, for an
    /// engine that has such a place.
    fn location(&self) -> Option<EngineLocation>;
}
