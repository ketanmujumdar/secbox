//! Adapter for the microsandbox CLI.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::argv;
use crate::error::{EngineError, Result};
use crate::process;
use crate::redact::{Chain, LiteralRedactor, NoRedaction, Redactor};
use crate::types::{
    CreateSpec, EngineLocation, ExecOutput, ExecRequest, GuestPath, LogOptions, OutputSink,
    SandboxName, SandboxStatus, StreamEnd,
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
        self.expect_success("msb create", None, done).map(|_| ())
    }

    fn start(&self, name: &SandboxName) -> Result<()> {
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
        let disk = self.home.join("sandboxes").join(name.as_str()).join("rootfs.raw");
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
        for script in ["echo first; exec sleep 60", "printf 'no newline'; exec sleep 60"] {
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
        let result = stub
            .engine()
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
