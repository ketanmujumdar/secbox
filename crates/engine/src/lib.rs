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
    CreateSpec, EngineLocation, ExecOutput, ExecRequest, GuestPath, LogOptions,
    NetworkPolicy, OutputSink, OutputStream, PortForward, ResizeSpec, Resources, SandboxMetrics, SandboxName, SandboxStatus,
    SecretRule, StreamEnd, TlsMode,
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
    /// Changes a stopped sandbox's limits, for its next start. Everything on its
    /// disk stays. The disk can only grow: anything else is an error and nothing
    /// changes. Memory and CPUs cannot be changed while it runs.
    fn resize(&self, name: &SandboxName, spec: &ResizeSpec) -> Result<()>;
    /// Changes a stopped sandbox's port forwards, for its next start: removes
    /// the forwards in `remove` and adds those in `add`. A running sandbox is
    /// refused, because the engine only applies a port change when it starts.
    fn set_ports(&self, _name: &SandboxName, _remove: &[PortForward], _add: &[PortForward]) -> Result<()> {
        Err(EngineError::NotSupported {
            operation: "set_ports",
            reason: "this engine cannot change a sandbox's ports",
        })
    }
    /// Reports what a running sandbox is using now.
    fn metrics(&self, name: &SandboxName) -> Result<SandboxMetrics>;
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

    /// Runs a command, writing `input` to its standard input.
    ///
    /// This is how a secret reaches the guest. The value is not in the
    /// argument list, so it cannot be read from the host's process list, and
    /// it is not written to a file on the host on the way.
    fn exec_with_input(
        &self,
        name: &SandboxName,
        request: &ExecRequest,
        input: &[u8],
    ) -> Result<ExecOutput>;
    /// Starts a command whose stdin and stdout are raw pipes, for a protocol
    /// the host relays byte for byte, such as Git's. Nothing on these pipes is
    /// filtered or redacted, so a caller must treat stdout as hostile bytes and
    /// never print it. The child has a cleared environment. A sandbox that is
    /// not running is refused, not booted.
    fn exec_piped(&self, _name: &SandboxName, _request: &ExecRequest) -> Result<std::process::Child> {
        Err(EngineError::NotSupported {
            operation: "exec_piped",
            reason: "this engine cannot relay a command's input and output",
        })
    }
    /// Copies a stopped sandbox's root disk to a host file, so a new sandbox can
    /// read it. The file is only ever copied on the host, never parsed. It is what
    /// lets a sandbox's network rules change, because the engine cannot change them
    /// on an existing sandbox, and its restore cannot set everything `create` sets.
    fn keep_root_disk(&self, _name: &SandboxName, _dest: &Path) -> Result<()> {
        Err(EngineError::NotSupported {
            operation: "keep_root_disk",
            reason: "this engine cannot keep a sandbox's disk",
        })
    }
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
