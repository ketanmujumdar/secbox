//! Adapter for the microsandbox CLI.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::argv;
use crate::error::{EngineError, Result};
use crate::process;
use crate::redact::{Chain, LiteralRedactor, NoRedaction, Redactor};
use crate::types::{
    CreateSpec, EngineLocation, ExecOutput, ExecRequest, GuestPath, LogOptions, OutputSink,
    ResizeSpec, SandboxMetrics, SandboxName, SandboxStatus, StreamEnd,
};
use crate::Engine;

/// The CLI version the adapter's behavior was checked against.
pub const TESTED_VERSION: &str = "0.7.13";

/// What the CLI adds to its state directory to form its shortest socket path.
/// Measured on v0.7.13: a 70-byte directory gave a 120-byte path.
pub const DERIVED_SOCKET_SUFFIX_BYTES: usize = 50;

/// macOS limit reported by the CLI: socket paths must be shorter than this.
const MACOS_SOCKET_LIMIT_BYTES: usize = 104;

/// Limit for quick CLI calls such as status.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Creating can pull an image.
const CREATE_TIMEOUT: Duration = Duration::from_secs(600);
const LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(120);
const COPY_TIMEOUT: Duration = Duration::from_secs(600);
/// Time beyond a command's own limit, so the CLI gets to report the timeout.
const EXEC_MARGIN: Duration = Duration::from_secs(5);

const NOT_FOUND_MARKER: &str = "sandbox not found";
const EXEC_TIMEOUT_MARKER: &str = "exec timed out";

/// Where the CLI is. The system `PATH` is never searched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MsbLocation {
    /// Absolute paths to the binary and to its state directory.
    Explicit { binary: PathBuf, home: PathBuf },
    /// The binary at `<project>/.tools/msb/bin/msb`. The state directory is separate
    /// because a project path is often too long for it: see [`MsbEngine::preflight`].
    ProjectTools {
        project_root: PathBuf,
        home: PathBuf,
    },
}

/// Runs the real CLI with fixed argument lists, without a shell.
pub struct MsbEngine {
    binary: PathBuf,
    home: PathBuf,
    timeout: Duration,
    redactor: Arc<dyn Redactor>,
    secret_env: BTreeMap<String, String>,
    secret_redactor: LiteralRedactor,
    /// The last CPU reading per sandbox, since use is worked out between two.
    cpu_samples: Mutex<HashMap<SandboxName, CpuSample>>,
}

// Hand-written so that secret values can never appear in debug output.
impl std::fmt::Debug for MsbEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MsbEngine")
            .field("binary", &self.binary)
            .field("home", &self.home)
            .field("timeout", &self.timeout)
            .field("secret_names", &self.secret_env.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

fn require_absolute(path: &Path, what: &str) -> Result<()> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(EngineError::BinaryNotFound {
            path: path.display().to_string(),
            reason: format!("{what} must be an absolute path"),
        })
    }
}

/// The state directory for a user: `<user home>/.<state dir>/msb`. A short path in
/// the home directory, since the usual macOS application support path is too long
/// for the CLI's sockets.
pub(crate) fn default_home(user_home: &Path) -> PathBuf {
    user_home
        .join(format!(".{}", brand::STATE_DIR_NAME))
        .join("msb")
}

impl MsbEngine {
    /// Checks that the binary exists. It does not run anything.
    pub fn new(location: MsbLocation) -> Result<Self> {
        let (binary, home) = match location {
            MsbLocation::Explicit { binary, home } => (binary, home),
            MsbLocation::ProjectTools { project_root, home } => (
                project_root
                    .join(".tools")
                    .join("msb")
                    .join("bin")
                    .join("msb"),
                home,
            ),
        };
        require_absolute(&binary, "the binary")?;
        require_absolute(&home, "the state directory")?;
        if !binary.is_file() {
            return Err(EngineError::BinaryNotFound {
                path: binary.display().to_string(),
                reason: "no such file".to_owned(),
            });
        }
        Ok(Self {
            binary,
            home,
            timeout: DEFAULT_TIMEOUT,
            redactor: Arc::new(NoRedaction),
            secret_env: BTreeMap::new(),
            secret_redactor: LiteralRedactor::default(),
            cpu_samples: Mutex::new(HashMap::new()),
        })
    }

    /// The default state directory for a user's home directory.
    pub fn default_home(user_home: &Path) -> PathBuf {
        default_home(user_home)
    }

    /// Sets the redactor applied to all command output and error text.
    #[must_use]
    pub fn with_redactor(mut self, redactor: Arc<dyn Redactor>) -> Self {
        self.redactor = redactor;
        self
    }

    /// Sets the limit for quick CLI calls. The process is killed when it is reached.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets the real values for [`crate::SecretRule`] names. They go only into the
    /// CLI's environment, never its arguments, and are always redacted from output.
    /// The CLI reads them at create and again at every start, so one engine needs
    /// them for the sandbox's whole life.
    #[must_use]
    pub fn with_secret_env(mut self, values: BTreeMap<String, String>) -> Self {
        self.secret_redactor = LiteralRedactor::new(values.values().cloned());
        self.secret_env = values;
        self
    }

    /// Fails if the CLI would refuse to create a sandbox because the state
    /// directory is too long for its sockets.
    pub fn preflight(&self) -> Result<()> {
        check_home_length(&self.home, cfg!(target_os = "macos"))
    }

    /// Returns the CLI's reported version, such as `0.7.13`.
    pub fn version(&self) -> Result<String> {
        let done = self.run("msb --version", &argv::version(), self.timeout)?;
        let text = self.expect_success("msb --version", None, done)?;
        let text = text.trim();
        Ok(text.strip_prefix("msb ").unwrap_or(text).to_owned())
    }

    /// Runs the CLI's host check and returns its report. v0.7.13 writes the report
    /// to stderr even on success, so both streams are returned.
    pub fn doctor(&self) -> Result<String> {
        let operation = "msb doctor";
        let done = self.run(operation, &argv::doctor(), self.timeout)?;
        if done.status.success() {
            Ok(done.stderr + &done.stdout)
        } else {
            Err(EngineError::CommandFailed {
                operation: operation.to_owned(),
                code: done.status.code(),
                stderr: done.stderr.trim().to_owned(),
            })
        }
    }

    fn env(&self) -> BTreeMap<String, String> {
        let mut env = self.secret_env.clone();
        env.insert(
            argv::HOME_VAR.to_owned(),
            self.home.to_string_lossy().into_owned(),
        );
        env
    }

    fn run(
        &self,
        operation: &str,
        args: &[String],
        timeout: Duration,
    ) -> Result<process::Finished> {
        process::run(
            &self.binary,
            args,
            &self.env(),
            timeout,
            operation,
            &Chain(self.redactor.as_ref(), &self.secret_redactor),
        )
    }

    /// Turns a non-zero exit into an error, and returns stdout otherwise.
    fn expect_success(
        &self,
        operation: &str,
        name: Option<&SandboxName>,
        done: process::Finished,
    ) -> Result<String> {
        if done.status.success() {
            return Ok(done.stdout);
        }
        if let Some(name) = name {
            if done.stderr.contains(NOT_FOUND_MARKER) {
                return Err(EngineError::NotFound {
                    name: name.to_string(),
                });
            }
        }
        Err(EngineError::CommandFailed {
            operation: operation.to_owned(),
            code: done.status.code(),
            stderr: done.stderr.trim().to_owned(),
        })
    }

    /// Applies [`seal_capture_logs`] to a sandbox, if the engine has made its
    /// folder, making the logs folder in it if that is missing. A sandbox with
    /// no folder has nothing to seal, and the engine's own answer to the
    /// command that follows says why.
    fn seal(&self, name: &SandboxName) -> Result<()> {
        let dirs = EngineLocation {
            binary: self.binary.clone(),
            home: self.home.clone(),
        }
        .sandbox_log_dirs(name);
        let failed = |e: std::io::Error| EngineError::Spawn {
            operation: "keep the sandbox's output off the host".to_owned(),
            reason: e.kind().to_string(),
        };
        // Nothing on the way may be a link: the guest's own shares are siblings.
        let (logs, above) = dirs
            .split_last()
            .expect("the list ends with the logs folder");
        for dir in above.iter().chain([logs]) {
            match std::fs::symlink_metadata(dir) {
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => {
                    return Err(failed(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "not a plain folder",
                    )))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && dir == logs => {
                    std::fs::create_dir(logs).map_err(failed)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(failed(e)),
            }
        }
        seal_capture_logs(logs).map_err(failed)
    }

    /// The CLI's `exec` boots a stopped sandbox for that one command, as seen on
    /// v0.7.13, and `copy` tries to. This refuses instead, so nothing boots unasked.
    fn require_running(&self, name: &SandboxName) -> Result<()> {
        match self.status(name)? {
            SandboxStatus::Running => Ok(()),
            SandboxStatus::NotFound => Err(EngineError::NotFound {
                name: name.to_string(),
            }),
            other => Err(EngineError::InvalidState {
                name: name.to_string(),
                reason: format!("is not running ({other:?})"),
            }),
        }
    }
}

/// Where the engine copies every byte any `exec` prints, as JSON lines.
pub const EXEC_LOG: &str = "exec.log";
/// Where the engine copies the guest's console, which root in the guest can
/// write anything to.
pub const KERNEL_LOG: &str = "kernel.log";

/// Stops the engine keeping, on the host, text the guest chooses.
///
/// Measured on v0.7.13: the engine writes every `exec`'s output to
/// `logs/exec.log`, so a secret a service prints, or that `logs` shows masked,
/// was kept there in plain text. It has no option to turn that off, but when
/// it cannot open the file it logs "capture disabled" and carries on, and
/// `exec` itself is unchanged. So a folder is put at that name. The console log
/// is opened at boot, and the start fails if it cannot be, so that one is
/// pointed at `/dev/null`.
///
/// Whatever was already in either file is cleared first. A running engine
/// keeps writing to the file it opened at boot until it stops, but that file
/// no longer has a name, so nothing can open it, and it is gone when the
/// engine closes it.
pub fn seal_capture_logs(logs: &Path) -> std::io::Result<()> {
    use std::fs;
    use std::io::ErrorKind;
    let exec = logs.join(EXEC_LOG);
    match fs::symlink_metadata(&exec) {
        Ok(meta) if meta.is_dir() => {}
        Ok(meta) => {
            empty_and_remove(&exec, &meta)?;
            make_private_dir(&exec)?;
        }
        Err(e) if e.kind() == ErrorKind::NotFound => make_private_dir(&exec)?,
        Err(e) => return Err(e),
    }
    let kernel = logs.join(KERNEL_LOG);
    let null = Path::new("/dev/null");
    match fs::symlink_metadata(&kernel) {
        Ok(meta) if meta.file_type().is_symlink() && fs::read_link(&kernel)? == null => {}
        Ok(meta) => {
            empty_and_remove(&kernel, &meta)?;
            std::os::unix::fs::symlink(null, &kernel)?;
        }
        Err(e) if e.kind() == ErrorKind::NotFound => std::os::unix::fs::symlink(null, &kernel)?,
        Err(e) => return Err(e),
    }
    Ok(())
}

fn make_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(0o700).create(path)
}

/// Empties a plain file with one name before removing it, so its content does
/// not live on in a file the engine still has open. Anything else is removed.
fn empty_and_remove(path: &Path, meta: &std::fs::Metadata) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    if meta.is_file() && meta.nlink() == 1 {
        let file = std::fs::OpenOptions::new().write(true).open(path)?;
        // The same file that was looked at, and not something put in its place.
        if file.metadata()?.ino() == meta.ino() {
            file.set_len(0)?;
        }
    }
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

fn check_home_length(home: &Path, is_macos: bool) -> Result<()> {
    if !is_macos {
        return Ok(());
    }
    let derived_bytes = home.as_os_str().len() + DERIVED_SOCKET_SUFFIX_BYTES;
    if derived_bytes < MACOS_SOCKET_LIMIT_BYTES {
        Ok(())
    } else {
        Err(EngineError::HomePathTooLong {
            home: home.display().to_string(),
            derived_bytes,
            limit_bytes: MACOS_SOCKET_LIMIT_BYTES,
        })
    }
}

fn parse_status(json: &str) -> Result<SandboxStatus> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| EngineError::CommandFailed {
            operation: "msb status".to_owned(),
            code: Some(0),
            stderr: format!("output was not JSON: {e}"),
        })?;
    match value.get("status").and_then(serde_json::Value::as_str) {
        Some("Running") => Ok(SandboxStatus::Running),
        Some("Stopped") => Ok(SandboxStatus::Stopped),
        Some(other) => Ok(SandboxStatus::Other(other.to_owned())),
        None => Err(EngineError::CommandFailed {
            operation: "msb status".to_owned(),
            code: Some(0),
            stderr: "output had no status field".to_owned(),
        }),
    }
}

/// One CPU reading: the VM's CPU time so far, how long it had been up, and
/// the use worked out when it was taken.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CpuSample {
    uptime_secs: f64,
    vcpu_time_ns: u64,
    cpu_millis: u32,
}

/// The counters a CPU figure is worked out from, as the engine reports them.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CpuCounters {
    uptime_secs: f64,
    vcpu_time_ns: u64,
    cpus: u32,
}

/// CPU in use between two readings, where 1000 is one whole CPU. The engine's
/// own `cpu_percent` is always 0, so this is worked out from its CPU time.
/// With no earlier reading, or after a restart set the counters back, there is
/// nothing to compare with, so it is 0 until the next reading. The engine
/// takes a reading about once a second, so two asks in that time get the same
/// one and the same answer.
fn cpu_millis_between(previous: Option<CpuSample>, now: CpuCounters) -> u32 {
    let Some(previous) = previous else { return 0 };
    if now.uptime_secs < previous.uptime_secs || now.vcpu_time_ns < previous.vcpu_time_ns {
        return 0;
    }
    let elapsed_secs = now.uptime_secs - previous.uptime_secs;
    if elapsed_secs <= 0.0 {
        return previous.cpu_millis;
    }
    #[allow(clippy::cast_precision_loss)]
    let busy_secs = (now.vcpu_time_ns - previous.vcpu_time_ns) as f64 / 1e9;
    let most = f64::from(now.cpus.max(1)) * 1000.0;
    // A saturating cast of a figure already held between 0 and the VM's CPUs.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let millis = (busy_secs / elapsed_secs * 1000.0).clamp(0.0, most).round() as u32;
    millis
}

fn parse_metrics(json: &str) -> Result<(SandboxMetrics, Option<CpuCounters>)> {
    let bad = |stderr: String| EngineError::CommandFailed {
        operation: "msb metrics".to_owned(),
        code: Some(0),
        stderr,
    };
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| bad(format!("output was not JSON: {e}")))?;
    let bytes = |field: &str| value.get(field).and_then(serde_json::Value::as_u64);
    let required =
        |field: &str| bytes(field).ok_or_else(|| bad(format!("output had no {field} field")));
    let percent = value
        .get("cpu_percent")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| bad("output had no cpu_percent field".to_owned()))?;
    let counters = match (
        value.get("uptime_secs").and_then(serde_json::Value::as_f64),
        bytes("vcpu_time_ns"),
        bytes("cpus").and_then(|c| u32::try_from(c).ok()),
    ) {
        (Some(uptime_secs), Some(vcpu_time_ns), Some(cpus)) => Some(CpuCounters {
            uptime_secs,
            vcpu_time_ns,
            cpus,
        }),
        _ => None,
    };
    let metrics = SandboxMetrics {
        // A saturating cast: a nonsense figure shows as a nonsense figure and
        // goes no further.
        cpu_millis: (percent * 10.0).round() as u32,
        memory_used_bytes: required("memory_bytes")?,
        memory_limit_bytes: required("memory_limit_bytes")?,
        memory_host_bytes: required("memory_host_resident_bytes")?,
        disk_used_bytes: bytes("upper_host_allocated_bytes"),
    };
    Ok((metrics, counters))
}

/// The engine answers a change it will not make, such as a smaller disk, with a
/// plan and `applied: false`, so the exit code alone does not say it was done.
fn check_resize(json: &str) -> Result<()> {
    let bad = |stderr: String| EngineError::CommandFailed {
        operation: "msb modify".to_owned(),
        code: Some(0),
        stderr,
    };
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| bad(format!("output was not JSON: {e}")))?;
    if value.get("applied").and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(());
    }
    let why = value
        .get("conflicts")
        .map(ToString::to_string)
        .unwrap_or_else(|| "no reason given".to_owned());
    Err(bad(format!("the change was not applied: {why}")))
}

impl Engine for MsbEngine {
    fn create(&self, spec: &CreateSpec) -> Result<()> {
        self.preflight()?;
        let args = argv::create(spec)?;
        for secret in &spec.secrets {
            if !self.secret_env.contains_key(&secret.env) {
                return Err(EngineError::InvalidInput {
                    field: "secret",
                    reason: format!("no value was given for {}", secret.env),
                });
            }
        }
        let done = self.run("msb create", &args, CREATE_TIMEOUT)?;
        if !done.status.success() && done.stderr.contains("already exists") {
            return Err(EngineError::AlreadyExists {
                name: spec.name.to_string(),
            });
        }
        self.expect_success("msb create", None, done)?;
        // `create` also boots it, so this comes as soon as it can. Nothing the
        // project runs has run yet. A sandbox that cannot be sealed is removed
        // rather than left to keep what its services print.
        if let Err(error) = self.seal(&spec.name) {
            let _ = self.destroy(&spec.name);
            return Err(error);
        }
        Ok(())
    }

    fn start(&self, name: &SandboxName) -> Result<()> {
        self.seal(name)?;
        let done = self.run("msb start", &argv::start(name), LIFECYCLE_TIMEOUT)?;
        self.expect_success("msb start", Some(name), done)
            .map(|_| ())
    }

    /// This takes well under a second and is not a clean shutdown for anything inside
    /// the guest: Postgres in a nested container ran crash recovery after a restart.
    /// Stop the workload first when that matters. Stopping twice is not an error.
    fn stop(&self, name: &SandboxName) -> Result<()> {
        let done = self.run("msb stop", &argv::stop(name), LIFECYCLE_TIMEOUT)?;
        self.expect_success("msb stop", Some(name), done)
            .map(|_| ())
    }

    fn destroy(&self, name: &SandboxName) -> Result<()> {
        let done = self.run("msb remove", &argv::destroy(name), LIFECYCLE_TIMEOUT)?;
        self.expect_success("msb remove", Some(name), done)
            .map(|_| ())
    }

    fn status(&self, name: &SandboxName) -> Result<SandboxStatus> {
        let done = self.run("msb status", &argv::status(name), self.timeout)?;
        if done.status.success() {
            return parse_status(&done.stdout);
        }
        if done.stderr.contains(NOT_FOUND_MARKER) {
            return Ok(SandboxStatus::NotFound);
        }
        Err(EngineError::CommandFailed {
            operation: "msb status".to_owned(),
            code: done.status.code(),
            stderr: done.stderr.trim().to_owned(),
        })
    }

    fn resize(&self, name: &SandboxName, spec: &ResizeSpec) -> Result<()> {
        let done = self.run("msb modify", &argv::resize(name, spec), LIFECYCLE_TIMEOUT)?;
        check_resize(&self.expect_success("msb modify", Some(name), done)?)
    }

    fn metrics(&self, name: &SandboxName) -> Result<SandboxMetrics> {
        let done = self.run("msb metrics", &argv::metrics(name), self.timeout)?;
        let (mut metrics, counters) =
            parse_metrics(&self.expect_success("msb metrics", Some(name), done)?)?;
        // An engine that does not report its CPU time keeps its own figure.
        if let Some(now) = counters {
            let mut samples = self
                .cpu_samples
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            metrics.cpu_millis = cpu_millis_between(samples.get(name).copied(), now);
            samples.insert(
                name.clone(),
                CpuSample {
                    uptime_secs: now.uptime_secs,
                    vcpu_time_ns: now.vcpu_time_ns,
                    cpu_millis: metrics.cpu_millis,
                },
            );
        }
        Ok(metrics)
    }

    /// A non-zero exit from the guest command is a normal result, not an error. The
    /// CLI's own failures are told apart by their messages, since it also exits 1.
    fn exec(&self, name: &SandboxName, request: &ExecRequest) -> Result<ExecOutput> {
        let args = argv::exec(name, request)?;
        self.require_running(name)?;
        let limit = request.timeout.unwrap_or(self.timeout) + EXEC_MARGIN;
        let done = self.run("msb exec", &args, limit)?;
        if done.stderr.contains(EXEC_TIMEOUT_MARKER) {
            return Err(EngineError::Timeout {
                operation: "msb exec".to_owned(),
                seconds: limit.as_secs().saturating_sub(EXEC_MARGIN.as_secs()),
            });
        }
        let Some(code) = done.status.code() else {
            return Err(EngineError::CommandFailed {
                operation: "msb exec".to_owned(),
                code: None,
                stderr: done.stderr.trim().to_owned(),
            });
        };
        Ok(ExecOutput {
            code,
            stdout: done.stdout,
            stderr: done.stderr,
        })
    }

    fn exec_stream(
        &self,
        name: &SandboxName,
        request: &ExecRequest,
        sink: &mut dyn OutputSink,
    ) -> Result<StreamEnd> {
        let args = argv::exec_stream(name, request)?;
        self.require_running(name)?;
        let limit = request.timeout.map(|timeout| timeout + EXEC_MARGIN);
        let (end, status) = process::stream(
            &self.binary,
            &args,
            &self.env(),
            limit,
            "msb exec",
            &Chain(self.redactor.as_ref(), &self.secret_redactor),
            sink,
        )?;
        if status.is_some_and(|status| status.code().is_none()) {
            return Err(EngineError::CommandFailed {
                operation: "msb exec".to_owned(),
                code: None,
                stderr: "the command was ended by a signal".to_owned(),
            });
        }
        Ok(end)
    }

    fn exec_with_input(
        &self,
        name: &SandboxName,
        request: &ExecRequest,
        input: &[u8],
    ) -> Result<ExecOutput> {
        let args = argv::exec_with_input(name, request)?;
        self.require_running(name)?;
        let limit = request.timeout.unwrap_or(self.timeout) + EXEC_MARGIN;
        let done = process::run_with_input(
            &self.binary,
            &args,
            &self.env(),
            limit,
            "msb exec",
            &Chain(self.redactor.as_ref(), &self.secret_redactor),
            Some(zeroize::Zeroizing::new(input.to_vec())),
        )?;
        if done.stderr.contains(EXEC_TIMEOUT_MARKER) {
            return Err(EngineError::Timeout {
                operation: "msb exec".to_owned(),
                seconds: limit.as_secs().saturating_sub(EXEC_MARGIN.as_secs()),
            });
        }
        let Some(code) = done.status.code() else {
            return Err(EngineError::CommandFailed {
                operation: "msb exec".to_owned(),
                code: None,
                stderr: done.stderr.trim().to_owned(),
            });
        };
        Ok(ExecOutput {
            code,
            stdout: done.stdout,
            stderr: done.stderr,
        })
    }

    fn exec_piped(&self, name: &SandboxName, request: &ExecRequest) -> Result<std::process::Child> {
        // The same arguments as a terminal-less `exec` on pipes, which ran on a
        // real VM. Both directions streaming at once has not.
        let args = argv::exec_terminal(name, request, false)?;
        self.require_running(name)?;
        std::process::Command::new(&self.binary)
            .args(&args)
            .env_clear()
            .envs(self.env())
            .current_dir("/")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| EngineError::Spawn {
                operation: "msb exec".to_owned(),
                reason: e.kind().to_string(),
            })
    }

    fn keep_root_disk(&self, name: &SandboxName, dest: &Path) -> Result<()> {
        // Named here because the CLI has no command for it. On a Mac this copy
        // shares its blocks with the original, so it is instant and costs little.
        let disk = self
            .home
            .join("sandboxes")
            .join(name.as_str())
            .join("rootfs.raw");
        let spawn_error = |e: std::io::Error| EngineError::Spawn {
            operation: "keep the sandbox's disk".to_owned(),
            reason: e.kind().to_string(),
        };
        let meta = std::fs::symlink_metadata(&disk).map_err(spawn_error)?;
        if !meta.is_file() {
            return Err(EngineError::InvalidInput {
                field: "root disk",
                reason: "the sandbox has no plain disk file to keep".to_owned(),
            });
        }
        let _ = std::fs::remove_file(dest);
        std::fs::copy(&disk, dest).map_err(spawn_error)?;
        Ok(())
    }

    fn exec_detached(&self, name: &SandboxName, request: &ExecRequest) -> Result<String> {
        let args = argv::exec_detached(name, request)?;
        self.require_running(name)?;
        let done = self.run("msb exec --detach", &args, self.timeout)?;
        let out = self.expect_success("msb exec --detach", Some(name), done)?;
        Ok(out.trim().to_owned())
    }

    fn copy_in(&self, name: &SandboxName, host: &Path, guest: &GuestPath) -> Result<()> {
        let args = argv::copy_in(name, host, guest)?;
        self.require_running(name)?;
        let done = self.run("msb copy", &args, COPY_TIMEOUT)?;
        self.expect_success("msb copy", Some(name), done)
            .map(|_| ())
    }

    fn logs(&self, name: &SandboxName, options: &LogOptions) -> Result<String> {
        let done = self.run("msb logs", &argv::logs(name, options), self.timeout)?;
        self.expect_success("msb logs", Some(name), done)
    }

    fn location(&self) -> Option<EngineLocation> {
        Some(EngineLocation {
            binary: self.binary.clone(),
            home: self.home.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{NetworkPolicy, Resources};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT_DIR: AtomicU32 = AtomicU32::new(0);

    /// A temporary directory that holds a stub CLI, removed on drop.
    struct Stub {
        dir: PathBuf,
        binary: PathBuf,
    }

    impl Stub {
        fn new(script: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "engine-test-{}-{}",
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::SeqCst)
            ));
            fs::create_dir_all(&dir).unwrap();
            let binary = dir.join("msb");
            fs::write(&binary, format!("#!/bin/sh\n{script}\n")).unwrap();
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
            Self { dir, binary }
        }

        fn engine(&self) -> MsbEngine {
            MsbEngine::new(MsbLocation::Explicit {
                binary: self.binary.clone(),
                // Not under the temp directory: on macOS that path alone is too long.
                home: PathBuf::from("/nonexistent/h"),
            })
            .unwrap()
        }
    }

    impl Drop for Stub {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn name() -> SandboxName {
        SandboxName::new("demo").unwrap()
    }

    #[test]
    fn project_tools_location_keeps_state_separate() {
        let dir = std::env::temp_dir().join(format!("engine-loc-{}", std::process::id()));
        let bin = dir.join(".tools/msb/bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("msb"), "").unwrap();
        let engine = MsbEngine::new(MsbLocation::ProjectTools {
            project_root: dir.clone(),
            home: "/state/msb".into(),
        })
        .unwrap();
        assert_eq!(engine.binary, dir.join(".tools/msb/bin/msb"));
        assert_eq!(engine.home, PathBuf::from("/state/msb"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn default_home_is_short_enough_for_a_typical_user() {
        let home = MsbEngine::default_home(Path::new("/Users/someone"));
        assert!(home.ends_with("msb"));
        assert!(home.starts_with("/Users/someone"));
        assert!(check_home_length(&home, true).is_ok());
    }

    #[test]
    fn missing_or_relative_binary_is_rejected() {
        for binary in ["/nonexistent/msb", "msb", "./msb"] {
            let result = MsbEngine::new(MsbLocation::Explicit {
                binary: binary.into(),
                home: "/h".into(),
            });
            assert!(
                matches!(result, Err(EngineError::BinaryNotFound { .. })),
                "{binary}"
            );
        }
    }

    #[test]
    fn relative_home_is_rejected() {
        let stub = Stub::new("exit 0");
        let result = MsbEngine::new(MsbLocation::Explicit {
            binary: stub.binary.clone(),
            home: "rel".into(),
        });
        assert!(matches!(result, Err(EngineError::BinaryNotFound { .. })));
    }

    #[test]
    fn home_length_matches_observed_limit() {
        let home = |n: usize| PathBuf::from(format!("/{}", "a".repeat(n - 1)));
        assert!(check_home_length(&home(53), true).is_ok());
        assert_eq!(
            check_home_length(&home(70), true),
            Err(EngineError::HomePathTooLong {
                home: home(70).display().to_string(),
                derived_bytes: 120,
                limit_bytes: 104,
            })
        );
        assert!(check_home_length(&home(54), true).is_err());
        assert!(check_home_length(&home(70), false).is_ok());
    }

    /// A sandbox folder as engine v0.7.13 leaves it after printing a secret.
    fn engine_logs(root: &Path) -> PathBuf {
        let logs = root.join("sandboxes").join("demo").join("logs");
        fs::create_dir_all(&logs).unwrap();
        fs::write(
            logs.join(EXEC_LOG),
            "{\"s\":\"stdout\",\"d\":\"token: fake-token-1234\\n\"}\n",
        )
        .unwrap();
        fs::write(logs.join(KERNEL_LOG), "consoletoken1234\n").unwrap();
        fs::write(logs.join("runtime.log"), "kept\n").unwrap();
        logs
    }

    #[test]
    fn sealing_clears_the_captured_output_and_leaves_nothing_to_write_it_to() {
        let dir = Stub::new("exit 0");
        let logs = engine_logs(&dir.dir);
        // The engine holds the file open while it runs.
        let held = fs::File::open(logs.join(EXEC_LOG)).unwrap();
        seal_capture_logs(&logs).unwrap();
        assert_eq!(
            held.metadata().unwrap().len(),
            0,
            "what was captured is cleared"
        );
        let exec = fs::symlink_metadata(logs.join(EXEC_LOG)).unwrap();
        assert!(exec.is_dir());
        assert_eq!(fs::read_dir(logs.join(EXEC_LOG)).unwrap().count(), 0);
        assert_eq!(
            fs::read_link(logs.join(KERNEL_LOG)).unwrap(),
            Path::new("/dev/null")
        );
        assert_eq!(
            fs::read_to_string(logs.join("runtime.log")).unwrap(),
            "kept\n"
        );
        // Twice is the same as once.
        seal_capture_logs(&logs).unwrap();
        assert!(fs::symlink_metadata(logs.join(EXEC_LOG)).unwrap().is_dir());
        assert_eq!(
            fs::read_link(logs.join(KERNEL_LOG)).unwrap(),
            Path::new("/dev/null")
        );
    }

    #[test]
    fn a_link_put_at_either_name_is_replaced_and_its_target_is_untouched() {
        let dir = Stub::new("exit 0");
        let logs = engine_logs(&dir.dir);
        let target = dir.dir.join("target");
        fs::write(&target, "mine\n").unwrap();
        for name in [EXEC_LOG, KERNEL_LOG] {
            fs::remove_file(logs.join(name)).unwrap();
            std::os::unix::fs::symlink(&target, logs.join(name)).unwrap();
        }
        seal_capture_logs(&logs).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "mine\n");
        assert!(fs::symlink_metadata(logs.join(EXEC_LOG)).unwrap().is_dir());
        assert_eq!(
            fs::read_link(logs.join(KERNEL_LOG)).unwrap(),
            Path::new("/dev/null")
        );
    }

    #[test]
    fn start_seals_the_logs_before_the_engine_runs() {
        let dir = Stub::new("exit 0");
        let logs = engine_logs(&dir.dir);
        let stub = Stub::new(&format!(
            "test -d '{}' || exit 9\necho started",
            logs.join(EXEC_LOG).display()
        ));
        let engine = MsbEngine::new(MsbLocation::Explicit {
            binary: stub.binary.clone(),
            home: dir.dir.clone(),
        })
        .unwrap();
        engine.start(&name()).unwrap();
        assert!(fs::symlink_metadata(logs.join(EXEC_LOG)).unwrap().is_dir());
    }

    #[test]
    fn version_strips_program_name() {
        let stub = Stub::new("echo 'msb 0.7.13'");
        assert_eq!(stub.engine().version().unwrap(), "0.7.13");
    }

    #[test]
    fn arguments_reach_the_cli_literally_without_a_shell() {
        let stub = Stub::new(r#"for a in "$@"; do printf '<%s>' "$a"; done"#);
        let args = vec!["$(touch /tmp/x)".to_owned(), "a b;c".to_owned()];
        let done = stub.engine().run("t", &args, DEFAULT_TIMEOUT).unwrap();
        assert_eq!(done.stdout, "<$(touch /tmp/x)><a b;c>");
    }

    #[test]
    fn child_gets_only_msb_home() {
        std::env::var_os("PATH").expect("the test needs PATH to be set");
        let stub = Stub::new(r#"echo "[$MSB_HOME][${PATH:-unset}]""#);
        let engine = stub.engine();
        let out = engine.doctor().unwrap();
        assert_eq!(
            out.trim(),
            format!("[{}][{}]", engine.home.display(), shell_default_path())
        );
    }

    /// `sh` supplies its own default PATH when given none, so the variable alone
    /// shows that the parent's value was not passed down.
    fn shell_default_path() -> String {
        let out = std::process::Command::new("/bin/sh")
            .env_clear()
            .args(["-c", "printf '%s' \"${PATH:-unset}\""])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    }

    #[test]
    fn failure_surfaces_redacted_stderr_and_code() {
        let stub = Stub::new("echo 'boom sk-fake-1 here' >&2; exit 7");
        let engine = stub
            .engine()
            .with_redactor(Arc::new(LiteralRedactor::new(["sk-fake-1"])));
        match engine.doctor().unwrap_err() {
            EngineError::CommandFailed {
                code,
                stderr,
                operation,
            } => {
                assert_eq!(code, Some(7));
                assert_eq!(operation, "msb doctor");
                assert_eq!(stderr, "boom [REDACTED] here");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn doctor_returns_a_report_written_to_stderr() {
        let stub = Stub::new("echo 'Host setup is ready.' >&2");
        assert!(stub.engine().doctor().unwrap().contains("ready"));
    }

    #[test]
    fn stdout_is_redacted_too() {
        let stub = Stub::new("echo 'value sk-fake-1'");
        let engine = stub
            .engine()
            .with_redactor(Arc::new(LiteralRedactor::new(["sk-fake-1"])));
        assert_eq!(engine.doctor().unwrap().trim(), "value [REDACTED]");
    }

    #[test]
    fn slow_command_is_killed_at_the_timeout() {
        let stub = Stub::new("exec sleep 30");
        let engine = stub.engine().with_timeout(Duration::from_millis(100));
        let started = std::time::Instant::now();
        assert!(matches!(engine.doctor(), Err(EngineError::Timeout { .. })));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn large_output_does_not_block() {
        let stub = Stub::new(
            "i=0; while [ $i -lt 3000 ]; do printf '%01024d\\n' 0; i=$((i+1)); done; exit 0",
        );
        let out = stub.engine().doctor().unwrap();
        assert_eq!(out.len() as u64, 1024 * 1024);
    }

    #[test]
    fn status_maps_observed_not_found() {
        let stub = Stub::new("echo 'error: sandbox not found: demo' >&2; exit 1");
        assert_eq!(
            stub.engine().status(&name()).unwrap(),
            SandboxStatus::NotFound
        );
    }

    #[test]
    fn status_other_failure_is_an_error() {
        let stub = Stub::new("echo 'error: db locked' >&2; exit 1");
        assert!(matches!(
            stub.engine().status(&name()),
            Err(EngineError::CommandFailed { .. })
        ));
    }

    /// Output of `msb metrics <name> --format json` from msb 0.7.13.
    const METRICS: &str = r#"{"cpu_percent":126.59,"cpus":2,"disk_read_bytes":1,"disk_write_bytes":2,
        "memory_available_bytes":3973107712,"memory_bytes":321859584,
        "memory_host_resident_bytes":209584128,"memory_limit_bytes":4294967296,"name":"demo",
        "net_rx_bytes":3,"net_tx_bytes":4,"state":"running","upper_free_bytes":null,
        "upper_host_allocated_bytes":786317312,"upper_used_bytes":null,"uptime_secs":1.5}"#;

    #[test]
    fn metrics_read_the_figures_the_engine_reports() {
        assert_eq!(
            parse_metrics(METRICS).unwrap().0,
            SandboxMetrics {
                cpu_millis: 1266,
                memory_used_bytes: 321_859_584,
                memory_limit_bytes: 4_294_967_296,
                memory_host_bytes: 209_584_128,
                disk_used_bytes: Some(786_317_312),
            }
        );
    }

    #[test]
    fn metrics_without_the_disk_figure_leave_only_that_out() {
        let json = METRICS.replace(
            "\"upper_host_allocated_bytes\":786317312",
            "\"upper_host_allocated_bytes\":null",
        );
        assert_eq!(parse_metrics(&json).unwrap().0.disk_used_bytes, None);
    }

    #[test]
    fn metrics_read_the_cpu_counters_when_the_engine_reports_them() {
        assert_eq!(parse_metrics(METRICS).unwrap().1, None);
        let json = METRICS.replace("\"cpus\":2,", "\"cpus\":2,\"vcpu_time_ns\":59033298916,");
        assert_eq!(
            parse_metrics(&json).unwrap().1,
            Some(CpuCounters {
                uptime_secs: 1.5,
                vcpu_time_ns: 59_033_298_916,
                cpus: 2
            })
        );
    }

    fn reading(uptime_secs: f64, vcpu_time_ns: u64) -> CpuCounters {
        CpuCounters {
            uptime_secs,
            vcpu_time_ns,
            cpus: 2,
        }
    }

    fn sample(uptime_secs: f64, vcpu_time_ns: u64, cpu_millis: u32) -> Option<CpuSample> {
        Some(CpuSample {
            uptime_secs,
            vcpu_time_ns,
            cpu_millis,
        })
    }

    #[test]
    fn cpu_is_cpu_time_over_elapsed_time() {
        // Half a CPU second over two seconds is a quarter of one CPU.
        assert_eq!(
            cpu_millis_between(sample(10.0, 1_000_000_000, 0), reading(12.0, 1_500_000_000)),
            250
        );
        // Two CPUs fully busy.
        assert_eq!(
            cpu_millis_between(sample(10.0, 0, 0), reading(11.0, 2_000_000_000)),
            2000
        );
    }

    #[test]
    fn cpu_with_nothing_to_compare_with_is_zero() {
        assert_eq!(cpu_millis_between(None, reading(12.0, 1_500_000_000)), 0);
        // A restart sets the counters back.
        assert_eq!(
            cpu_millis_between(sample(500.0, 9_000_000_000, 700), reading(3.0, 100)),
            0
        );
        assert_eq!(
            cpu_millis_between(sample(5.0, 9_000_000_000, 700), reading(6.0, 100)),
            0
        );
    }

    #[test]
    fn cpu_from_the_same_reading_repeats_the_last_figure() {
        assert_eq!(
            cpu_millis_between(sample(10.0, 5, 430), reading(10.0, 5)),
            430
        );
    }

    #[test]
    fn cpu_never_shows_more_than_the_vm_has() {
        assert_eq!(
            cpu_millis_between(sample(10.0, 0, 0), reading(11.0, 9_000_000_000)),
            2000
        );
    }

    #[test]
    fn metrics_that_are_not_figures_are_an_error_rather_than_zero() {
        assert!(parse_metrics("not json").is_err());
        assert!(parse_metrics("{\"cpu_percent\":1.0}").is_err());
    }

    #[test]
    fn a_resize_the_engine_did_not_apply_is_an_error() {
        assert!(check_resize(r#"{"applied":true,"conflicts":[]}"#).is_ok());
        let refused = check_resize(r#"{"applied":false,"conflicts":["root disk can only grow"]}"#)
            .unwrap_err();
        assert!(refused.to_string().contains("can only grow"), "{refused}");
        assert!(check_resize("{}").is_err());
    }

    #[test]
    fn resize_argv_names_the_disk_only_when_it_grows() {
        let mut spec = ResizeSpec {
            memory_mib: 6144,
            cpus: 4,
            grow_disk_gib: None,
        };
        assert_eq!(
            argv::resize(&name(), &spec),
            ["modify", "demo", "--memory", "6144M", "--cpus", "4", "--format", "json"]
        );
        spec.grow_disk_gib = Some(30);
        assert_eq!(
            argv::resize(&name(), &spec),
            [
                "modify",
                "demo",
                "--memory",
                "6144M",
                "--cpus",
                "4",
                "--root-disk",
                "30G",
                "--format",
                "json"
            ]
        );
    }

    const RUNNING: &str = r#"echo '{"name":"demo","status":"Running"}'"#;

    /// A stub that reports `status` as running and otherwise runs `rest`.
    fn running_stub(rest: &str) -> Stub {
        Stub::new(&format!(
            "if [ \"$1\" = status ]; then {RUNNING}; exit 0; fi\n{rest}"
        ))
    }

    #[test]
    fn status_parses_observed_states() {
        assert_eq!(
            parse_status(r#"{"status":"Running"}"#),
            Ok(SandboxStatus::Running)
        );
        assert_eq!(
            parse_status(r#"{"status":"Stopped"}"#),
            Ok(SandboxStatus::Stopped)
        );
        assert_eq!(
            parse_status(r#"{"status":"Crashed"}"#),
            Ok(SandboxStatus::Other("Crashed".to_owned()))
        );
        assert!(parse_status("nope").is_err());
        assert!(parse_status("{}").is_err());
    }

    fn create_spec() -> CreateSpec {
        CreateSpec {
            name: name(),
            image: "docker:dind".to_owned(),
            resources: Resources::new(2048, 2, 10).unwrap(),
            network: NetworkPolicy::DenyAll,
            secrets: vec![],
            ports: vec![],
            previous_disk: None,
        }
    }

    #[test]
    fn create_runs_the_built_argv() {
        let stub = Stub::new(r#"printf '%s\n' "$@" > "$(dirname "$0")/args""#);
        stub.engine().create(&create_spec()).unwrap();
        let args = fs::read_to_string(stub.dir.join("args")).unwrap();
        assert!(args.starts_with("create\n--quiet\n--name\ndemo\n"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn create_stops_at_preflight_without_running_the_cli() {
        let stub = Stub::new(r#"touch "$(dirname "$0")/ran""#);
        let engine = MsbEngine::new(MsbLocation::Explicit {
            binary: stub.binary.clone(),
            home: PathBuf::from(format!("/{}", "a".repeat(70))),
        })
        .unwrap();
        assert!(matches!(
            engine.create(&create_spec()),
            Err(EngineError::HomePathTooLong { .. })
        ));
        assert!(!stub.dir.join("ran").exists());
    }

    #[test]
    fn secret_value_goes_to_the_environment_not_the_arguments() {
        let stub = Stub::new(
            r#"printf '%s\n' "$@" > "$(dirname "$0")/args"; printf '%s' "$API_KEY" > "$(dirname "$0")/val""#,
        );
        let engine = stub.engine().with_secret_env(BTreeMap::from([(
            "API_KEY".to_owned(),
            "sk-fake-2".to_owned(),
        )]));
        let mut spec = create_spec();
        spec.network = NetworkPolicy::AllowHosts {
            hosts: vec!["a.example.com".into()],
            tls: crate::types::TlsMode::Intercept,
        };
        spec.secrets = vec![crate::types::SecretRule {
            env: "API_KEY".into(),
            hosts: vec!["a.example.com".into()],
        }];
        engine.create(&spec).unwrap();
        assert_eq!(
            fs::read_to_string(stub.dir.join("val")).unwrap(),
            "sk-fake-2"
        );
        let args = fs::read_to_string(stub.dir.join("args")).unwrap();
        assert!(args.contains("API_KEY@a.example.com"));
        assert!(!args.contains("sk-fake-2"));
    }

    #[test]
    fn create_refuses_a_secret_with_no_value() {
        let stub = Stub::new(r#"touch "$(dirname "$0")/ran""#);
        let mut spec = create_spec();
        spec.network = NetworkPolicy::AllowHosts {
            hosts: vec!["a.example.com".into()],
            tls: crate::types::TlsMode::Intercept,
        };
        spec.secrets = vec![crate::types::SecretRule {
            env: "API_KEY".into(),
            hosts: vec!["a.example.com".into()],
        }];
        assert!(matches!(
            stub.engine().create(&spec),
            Err(EngineError::InvalidInput { .. })
        ));
        assert!(!stub.dir.join("ran").exists());
    }

    #[test]
    fn secret_values_are_always_redacted_and_hidden_from_debug() {
        let stub = Stub::new("echo 'leak sk-fake-3' >&2; exit 1");
        let engine = stub.engine().with_secret_env(BTreeMap::from([(
            "API_KEY".to_owned(),
            "sk-fake-3".to_owned(),
        )]));
        let err = engine.doctor().unwrap_err().to_string();
        assert!(!err.contains("sk-fake-3"), "{err}");
        assert!(err.contains("[REDACTED]"));
        assert!(!format!("{engine:?}").contains("sk-fake-3"));
    }

    #[test]
    fn exec_reports_guest_exit_codes_as_results() {
        let stub = running_stub("echo out; echo err >&2; exit 7");
        let out = stub
            .engine()
            .exec(&name(), &ExecRequest::new(["x"]))
            .unwrap();
        assert_eq!(
            out,
            ExecOutput {
                code: 7,
                stdout: "out\n".to_owned(),
                stderr: "err\n".to_owned(),
            }
        );
    }

    #[test]
    fn exec_reports_the_clis_timeout_as_a_timeout() {
        let stub = running_stub("echo 'error: exec timed out after 1s' >&2; exit 1");
        assert!(matches!(
            stub.engine().exec(&name(), &ExecRequest::new(["x"])),
            Err(EngineError::Timeout { .. })
        ));
    }

    #[test]
    fn exec_and_copy_refuse_a_stopped_sandbox_without_running_them() {
        let stub = Stub::new(
            r#"if [ "$1" = status ]; then echo '{"status":"Stopped"}'; exit 0; fi
touch "$(dirname "$0")/ran""#,
        );
        let engine = stub.engine();
        let n = name();
        let guest = GuestPath::new("/w").unwrap();
        assert!(matches!(
            engine.exec(&n, &ExecRequest::new(["x"])),
            Err(EngineError::InvalidState { .. })
        ));
        assert!(matches!(
            engine.exec_detached(&n, &ExecRequest::new(["x"])),
            Err(EngineError::InvalidState { .. })
        ));
        assert!(matches!(
            engine.copy_in(&n, Path::new("/tmp/x"), &guest),
            Err(EngineError::InvalidState { .. })
        ));
        assert!(!stub.dir.join("ran").exists());
    }

    #[derive(Default)]
    struct Collect {
        out: Vec<u8>,
        err: Vec<u8>,
        stop_after_first: bool,
    }

    impl OutputSink for Collect {
        fn output(&mut self, stream: crate::types::OutputStream, bytes: &[u8]) -> bool {
            match stream {
                crate::types::OutputStream::Stdout => self.out.extend_from_slice(bytes),
                crate::types::OutputStream::Stderr => self.err.extend_from_slice(bytes),
            }
            !self.stop_after_first
        }
    }

    #[test]
    fn a_stream_separates_output_and_reports_the_code() {
        let stub = running_stub("echo out; echo err >&2; exit 7");
        let mut sink = Collect::default();
        let end = stub
            .engine()
            .exec_stream(&name(), &ExecRequest::new(["x"]), &mut sink)
            .unwrap();
        assert_eq!(end, StreamEnd::Exited(7));
        assert_eq!(sink.out, b"out\n");
        assert_eq!(sink.err, b"err\n");
    }

    /// Following is no use if nothing shows until the command ends. The command
    /// here outlives the test's patience, so only early delivery can pass.
    #[test]
    fn a_stream_delivers_output_before_the_command_ends() {
        for script in [
            "echo first; exec sleep 60",
            "printf 'no newline'; exec sleep 60",
        ] {
            let stub = running_stub(script);
            let mut sink = Collect {
                stop_after_first: true,
                ..Collect::default()
            };
            let started = std::time::Instant::now();
            let end = stub
                .engine()
                .exec_stream(&name(), &ExecRequest::new(["x"]), &mut sink)
                .unwrap();
            assert_eq!(end, StreamEnd::Stopped, "{script}");
            assert!(started.elapsed() < Duration::from_secs(30), "{script}");
            assert!(!sink.out.is_empty(), "{script}");
        }
    }

    #[test]
    fn whole_lines_keep_their_order_and_a_late_line_is_not_lost() {
        let stub = running_stub("echo first; sleep 1; echo second");
        let mut sink = Collect::default();
        stub.engine()
            .exec_stream(&name(), &ExecRequest::new(["x"]), &mut sink)
            .unwrap();
        assert_eq!(sink.out, b"first\nsecond\n");
    }

    #[test]
    fn a_stream_stops_a_command_when_the_sink_says_so() {
        let stub = running_stub("echo first; exec sleep 30");
        let mut sink = Collect {
            stop_after_first: true,
            ..Collect::default()
        };
        let started = std::time::Instant::now();
        let end = stub
            .engine()
            .exec_stream(&name(), &ExecRequest::new(["x"]), &mut sink)
            .unwrap();
        assert_eq!(end, StreamEnd::Stopped);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_quiet_stream_stops_when_its_reader_has_gone() {
        struct Gone;
        impl OutputSink for Gone {
            fn output(&mut self, _: crate::types::OutputStream, _: &[u8]) -> bool {
                true
            }
            fn cancelled(&self) -> bool {
                true
            }
        }
        let stub = running_stub("exec sleep 30");
        let started = std::time::Instant::now();
        let end = stub
            .engine()
            .exec_stream(&name(), &ExecRequest::new(["x"]), &mut Gone)
            .unwrap();
        assert_eq!(end, StreamEnd::Stopped);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_stream_with_a_limit_is_stopped_at_it() {
        let stub = running_stub("exec sleep 30");
        let mut request = ExecRequest::new(["x"]);
        request.timeout = Some(Duration::from_secs(0));
        let started = std::time::Instant::now();
        let result = stub
            .engine()
            .exec_stream(&name(), &request, &mut Collect::default());
        assert!(matches!(result, Err(EngineError::Timeout { .. })));
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn a_stream_with_no_limit_passes_no_timeout_to_the_cli() {
        let stub = running_stub(r#"printf '%s\n' "$@" > "$(dirname "$0")/args""#);
        stub.engine()
            .exec_stream(&name(), &ExecRequest::new(["x"]), &mut Collect::default())
            .unwrap();
        let args = fs::read_to_string(stub.dir.join("args")).unwrap();
        assert!(!args.contains("--timeout"), "{args}");
        assert!(args.starts_with("exec\n--stream\n--no-tty\n"), "{args}");
    }

    #[test]
    fn a_stream_is_redacted_even_when_a_value_arrives_in_two_pieces() {
        let stub = running_stub("printf 'value sk-fa'; printf 'ke-1 end\\n'");
        let engine = stub
            .engine()
            .with_redactor(Arc::new(LiteralRedactor::new(["sk-fake-1"])));
        let mut sink = Collect::default();
        engine
            .exec_stream(&name(), &ExecRequest::new(["x"]), &mut sink)
            .unwrap();
        assert_eq!(sink.out, b"value [REDACTED] end\n");
    }

    #[test]
    fn a_stream_refuses_a_stopped_sandbox_without_running_it() {
        let stub = Stub::new(
            r#"if [ "$1" = status ]; then echo '{"status":"Stopped"}'; exit 0; fi
touch "$(dirname "$0")/ran""#,
        );
        let result =
            stub.engine()
                .exec_stream(&name(), &ExecRequest::new(["x"]), &mut Collect::default());
        assert!(matches!(result, Err(EngineError::InvalidState { .. })));
        assert!(!stub.dir.join("ran").exists());
    }

    #[test]
    fn the_engine_says_where_it_is() {
        let stub = Stub::new("exit 0");
        let location = stub.engine().location().unwrap();
        assert_eq!(location.binary, stub.binary);
        assert_eq!(location.home, PathBuf::from("/nonexistent/h"));
    }

    #[test]
    fn input_reaches_the_command_without_appearing_in_its_arguments() {
        let stub = running_stub(
            r#"cat > "$(dirname "$0")/stdin"; printf '%s\n' "$@" > "$(dirname "$0")/args""#,
        );
        let out = stub
            .engine()
            .exec_with_input(
                &name(),
                &ExecRequest::new(["sh", "-c", "cat > /run/x"]),
                b"sk-fake-value",
            )
            .unwrap();
        assert_eq!(out.code, 0);
        assert_eq!(
            fs::read_to_string(stub.dir.join("stdin")).unwrap(),
            "sk-fake-value"
        );
        let args = fs::read_to_string(stub.dir.join("args")).unwrap();
        assert!(!args.contains("sk-fake-value"), "{args}");
        // The flag that closes standard input must not be there.
        assert!(!args.contains("--no-stdin"), "{args}");
    }

    #[test]
    fn input_is_refused_for_a_sandbox_that_is_not_running() {
        let stub = Stub::new(
            r#"if [ "$1" = status ]; then echo '{"status":"Stopped"}'; exit 0; fi
touch "$(dirname "$0")/ran""#,
        );
        assert!(matches!(
            stub.engine()
                .exec_with_input(&name(), &ExecRequest::new(["x"]), b"secret"),
            Err(EngineError::InvalidState { .. })
        ));
        assert!(!stub.dir.join("ran").exists());
    }

    #[test]
    fn exec_detached_returns_the_job_id() {
        let stub = running_stub("echo job_abc");
        assert_eq!(
            stub.engine()
                .exec_detached(&name(), &ExecRequest::new(["dockerd"]))
                .unwrap(),
            "job_abc"
        );
    }

    #[test]
    fn lifecycle_maps_not_found_and_other_failures() {
        let stub = Stub::new("echo 'error: sandbox not found: demo' >&2; exit 1");
        let e = stub.engine();
        for result in [e.start(&name()), e.stop(&name()), e.destroy(&name())] {
            assert!(matches!(result, Err(EngineError::NotFound { .. })));
        }
        assert!(matches!(
            e.logs(&name(), &LogOptions::default()),
            Err(EngineError::NotFound { .. })
        ));
        let stub = Stub::new("echo 'error: disk full' >&2; exit 1");
        assert!(matches!(
            stub.engine().start(&name()),
            Err(EngineError::CommandFailed { stderr, .. }) if stderr.contains("disk full")
        ));
    }

    #[test]
    fn logs_return_stdout() {
        let stub = Stub::new("echo line");
        assert_eq!(
            stub.engine().logs(&name(), &LogOptions::default()).unwrap(),
            "line\n"
        );
    }
}
