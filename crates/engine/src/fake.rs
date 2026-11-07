//! An in-memory engine for tests. It follows the same lifecycle rules a real
//! engine should, and records every call.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::error::{EngineError, Result};
use crate::types::{
    CreateSpec, EngineLocation, ExecOutput, ExecRequest, GuestPath, LogOptions, OutputSink,
    OutputStream, ResizeSpec, SandboxMetrics, SandboxName, SandboxStatus, StreamEnd,
};
use crate::Engine;

/// A call made to a [`FakeEngine`], in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeCall {
    Create(CreateSpec),
    Start(SandboxName),
    Stop(SandboxName),
    Destroy(SandboxName),
    KeepRootDisk(SandboxName, PathBuf),
    Status(SandboxName),
    Resize(SandboxName, ResizeSpec),
    Metrics(SandboxName),
    Exec(SandboxName, ExecRequest),
    ExecStream(SandboxName, ExecRequest),
    /// The input is deliberately not recorded here, so that a secret cannot
    /// be printed by a failing test. Read it with [`FakeEngine::inputs`].
    ExecWithInput(SandboxName, ExecRequest),
    ExecDetached(SandboxName, ExecRequest),
    ExecPiped(SandboxName, ExecRequest),
    CopyIn(SandboxName, PathBuf, GuestPath),
    Logs(SandboxName, LogOptions),
}

#[derive(Debug)]
struct Box_ {
    running: bool,
    spec: CreateSpec,
    copied: Vec<(PathBuf, GuestPath)>,
    /// The contents of the regular files copied in, by guest path, as they
    /// were when they were copied. A real copy reads them then too.
    files: BTreeMap<String, Vec<u8>>,
    log: Vec<String>,
}

#[derive(Debug, Default)]
struct State {
    boxes: BTreeMap<SandboxName, Box_>,
    calls: Vec<FakeCall>,
    exec_results: VecDeque<ExecOutput>,
    next_error: Option<EngineError>,
    stream_chunks: VecDeque<(OutputStream, Vec<u8>)>,
    stream_code: i32,
    stream_stays_open: bool,
    streams_stopped: usize,
    location: Option<EngineLocation>,
    /// Where `/work` is on this machine, for [`Engine::exec_piped`]. With one
    /// set, piped commands really run, locally, in that folder.
    piped_root: Option<PathBuf>,
    inputs: Vec<(Vec<String>, String)>,
    keeps_files: bool,
    lookups_denied: u32,
    refuses_resizes: bool,
}

/// In-memory [`Engine`]. `create` leaves a sandbox running, matching the CLI's
/// documented "create and boot" behavior.
#[derive(Debug, Default)]
pub struct FakeEngine {
    state: Mutex<State>,
}

impl FakeEngine {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic in another test thread must not hide this test's own result.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queues the output of a future `exec`. With none queued, `exec` succeeds
    /// with empty output.
    pub fn push_exec_result(&self, output: ExecOutput) {
        self.lock().exec_results.push_back(output);
    }

    /// Queues output for the next `exec_stream`.
    pub fn push_stream_output(&self, stream: OutputStream, bytes: &[u8]) {
        self.lock().stream_chunks.push_back((stream, bytes.to_vec()));
    }

    /// Sets the code the next `exec_stream` exits with.
    pub fn set_stream_code(&self, code: i32) {
        self.lock().stream_code = code;
    }

    /// Makes `exec_stream` behave like following a log: after its output, it
    /// stays open until its sink stops it.
    pub fn keep_streams_open(&self) {
        self.lock().stream_stays_open = true;
    }

    /// How many streams were stopped by their sink rather than exiting.
    pub fn streams_stopped(&self) -> usize {
        self.lock().streams_stopped
    }

    /// Sets what [`Engine::location`] reports. With none set, there is no
    /// place a client could run this engine from.
    pub fn set_location(&self, location: EngineLocation) {
        self.lock().location = Some(location);
    }

    /// Makes Git really run, on this machine, in `root`, standing in for the
    /// guest's `/work`: every [`Engine::exec_piped`], and every `exec` whose
    /// command is `git ...` or a `sh -c` script that runs Git. Anything else,
    /// such as Docker, still gets the canned answers. The commands get a
    /// cleared environment with only `PATH` and `HOME` (set to `root`), so a
    /// test can drive a real protocol through the code that relays it.
    pub fn run_piped_in(&self, root: &Path) {
        self.lock().piped_root = Some(root.to_path_buf());
    }

    /// Makes this engine keep files under its location as the real one does: the
    /// logs and guest state appear on create and go on destroy.
    pub fn keep_files(&self) {
        self.lock().keeps_files = true;
    }

    /// Where this sandbox's log folder is, for a test that keeps files.
    pub fn logs_dir(&self, name: &SandboxName) -> Option<PathBuf> {
        self.lock()
            .location
            .as_ref()
            .and_then(|location| location.sandbox_log_dirs(name).pop())
    }

    /// The guest looks up a name its network does not allow. As with the real
    /// engine, this is refused either way, and it leaves a line in the log only
    /// if the sandbox was created at a level that writes one.
    pub fn deny_lookup(&self, name: &SandboxName, domain: &str) -> Result<()> {
        let mut state = self.lock();
        let entry = state.boxes.get(name).ok_or_else(|| not_found(name))?;
        let args = crate::argv::create(&entry.spec)?;
        let writes = args
            .windows(2)
            .any(|pair| pair[0] == "--log-level" && (pair[1] == "debug" || pair[1] == "trace"));
        state.lookups_denied += 1;
        let tick = state.lookups_denied;
        let location = state.location.clone();
        drop(state);
        if !writes {
            return Ok(());
        }
        let line = format!(
            "2026-11-05T07:50:{:02}.{tick:06}Z DEBUG microsandbox_network::engine::dns::forwarder: DNS query denied by network policy domain={domain}\n",
            tick / 60 % 60
        );
        let logs = location
            .and_then(|location| location.sandbox_log_dirs(name).pop())
            .ok_or(EngineError::NotSupported {
                operation: "deny_lookup",
                reason: "this fake has no location",
            })?;
        let io = |e: std::io::Error| EngineError::InvalidInput {
            field: "fake log",
            reason: e.to_string(),
        };
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(logs.join("runtime.log"))
            .map_err(io)?;
        std::io::Write::write_all(&mut file, line.as_bytes()).map_err(io)
    }

    /// Makes the next operation fail with this error. The call is still recorded.
    pub fn fail_next(&self, error: EngineError) {
        self.lock().next_error = Some(error);
    }

    /// Makes every resize fail, for a test of what happens around one, since
    /// other calls come before it in a start.
    pub fn refuse_resizes(&self) {
        self.lock().refuses_resizes = true;
    }

    /// Adds a line to a sandbox's log.
    pub fn push_log(&self, name: &SandboxName, line: impl Into<String>) -> Result<()> {
        let mut state = self.lock();
        let entry = state.boxes.get_mut(name).ok_or_else(|| not_found(name))?;
        entry.log.push(line.into());
        Ok(())
    }

    /// What was written to the standard input of each command, in order, as
    /// pairs of command and input.
    pub fn inputs(&self) -> Vec<(Vec<String>, String)> {
        self.lock().inputs.clone()
    }

    /// All calls so far, in order.
    pub fn calls(&self) -> Vec<FakeCall> {
        self.lock().calls.clone()
    }

    /// The spec a sandbox was created with.
    pub fn spec(&self, name: &SandboxName) -> Option<CreateSpec> {
        self.lock().boxes.get(name).map(|b| b.spec.clone())
    }

    /// What has been copied into a sandbox.
    pub fn copied(&self, name: &SandboxName) -> Vec<(PathBuf, GuestPath)> {
        self.lock()
            .boxes
            .get(name)
            .map(|b| b.copied.clone())
            .unwrap_or_default()
    }

    /// A file's contents as they were copied in, by its path in the guest.
    pub fn copied_file(&self, name: &SandboxName, guest: &str) -> Option<Vec<u8>> {
        self.lock()
            .boxes
            .get(name)
            .and_then(|b| b.files.get(guest).cloned())
    }

    /// Records the call and consumes any error queued by [`Self::fail_next`].
    fn begin(&self, call: FakeCall) -> Result<MutexGuard<'_, State>> {
        let mut state = self.lock();
        state.calls.push(call);
        match state.next_error.take() {
            Some(error) => Err(error),
            None => Ok(state),
        }
    }
}

/// How long a stream left open waits for its sink before giving up.
const FAKE_STREAM_LIMIT: Duration = Duration::from_secs(20);

fn not_found(name: &SandboxName) -> EngineError {
    EngineError::NotFound {
        name: name.to_string(),
    }
}

fn running_box<'a>(state: &'a mut State, name: &SandboxName) -> Result<&'a mut Box_> {
    let entry = state.boxes.get_mut(name).ok_or_else(|| not_found(name))?;
    if entry.running {
        Ok(entry)
    } else {
        Err(EngineError::InvalidState {
            name: name.to_string(),
            reason: "is not running".to_owned(),
        })
    }
}

impl Engine for FakeEngine {
    fn create(&self, spec: &CreateSpec) -> Result<()> {
        let mut state = self.begin(FakeCall::Create(spec.clone()))?;
        // Uses the real builder so that bad input fails here as it would there.
        crate::argv::create(spec)?;
        if state.boxes.contains_key(&spec.name) {
            return Err(EngineError::AlreadyExists {
                name: spec.name.to_string(),
            });
        }
        if state.keeps_files {
            if let Some(location) = &state.location {
                if let Some(logs) = location.sandbox_log_dirs(&spec.name).pop() {
                    std::fs::create_dir_all(&logs).map_err(|e| EngineError::InvalidInput {
                        field: "fake files",
                        reason: e.to_string(),
                    })?;
                    let guest = logs.with_file_name("guest-state");
                    std::fs::create_dir_all(guest).map_err(|e| EngineError::InvalidInput {
                        field: "fake files",
                        reason: e.to_string(),
                    })?;
                }
            }
        }
        state.boxes.insert(
            spec.name.clone(),
            Box_ {
                running: true,
                spec: spec.clone(),
                copied: Vec::new(),
                files: BTreeMap::new(),
                log: Vec::new(),
            },
        );
        Ok(())
    }

    fn start(&self, name: &SandboxName) -> Result<()> {
        let mut state = self.begin(FakeCall::Start(name.clone()))?;
        let entry = state.boxes.get_mut(name).ok_or_else(|| not_found(name))?;
        if entry.running {
            return Err(EngineError::InvalidState {
                name: name.to_string(),
                reason: "is already running".to_owned(),
            });
        }
        entry.running = true;
        Ok(())
    }

    fn stop(&self, name: &SandboxName) -> Result<()> {
        let mut state = self.begin(FakeCall::Stop(name.clone()))?;
        running_box(&mut state, name)?.running = false;
        Ok(())
    }

    fn keep_root_disk(&self, name: &SandboxName, dest: &Path) -> Result<()> {
        let state = self.begin(FakeCall::KeepRootDisk(name.clone(), dest.to_path_buf()))?;
        let sandbox = state.boxes.get(name).ok_or_else(|| not_found(name))?;
        if sandbox.running {
            return Err(EngineError::InvalidInput {
                field: "root disk",
                reason: "a running sandbox's disk cannot be kept".to_owned(),
            });
        }
        std::fs::write(dest, b"fake disk").map_err(|e| EngineError::Spawn {
            operation: "fake keep".to_owned(),
            reason: e.kind().to_string(),
        })
    }

    fn destroy(&self, name: &SandboxName) -> Result<()> {
        let mut state = self.begin(FakeCall::Destroy(name.clone()))?;
        state.boxes.remove(name).ok_or_else(|| not_found(name))?;
        if state.keeps_files {
            if let Some(location) = &state.location {
                let dirs = location.sandbox_log_dirs(name);
                let _ = std::fs::remove_dir_all(&dirs[2]);
            }
        }
        Ok(())
    }

    fn status(&self, name: &SandboxName) -> Result<SandboxStatus> {
        let state = self.begin(FakeCall::Status(name.clone()))?;
        Ok(match state.boxes.get(name) {
            None => SandboxStatus::NotFound,
            Some(b) if b.running => SandboxStatus::Running,
            Some(_) => SandboxStatus::Stopped,
        })
    }

    fn resize(&self, name: &SandboxName, spec: &ResizeSpec) -> Result<()> {
        let mut state = self.begin(FakeCall::Resize(name.clone(), *spec))?;
        if state.refuses_resizes {
            return Err(EngineError::InvalidInput {
                field: "resize",
                reason: "refused".to_owned(),
            });
        }
        let entry = state.boxes.get_mut(name).ok_or_else(|| not_found(name))?;
        if entry.running {
            return Err(EngineError::InvalidState {
                name: name.to_string(),
                reason: "is running, and cannot be resized while it runs".to_owned(),
            });
        }
        // The real engine refuses the same size as well as a smaller one.
        if let Some(disk) = spec.grow_disk_gib {
            if disk <= entry.spec.resources.disk_gib {
                return Err(EngineError::InvalidInput {
                    field: "disk",
                    reason: "a disk can only grow".to_owned(),
                });
            }
            entry.spec.resources.disk_gib = disk;
        }
        entry.spec.resources.memory_mib = spec.memory_mib;
        entry.spec.resources.cpus = spec.cpus;
        Ok(())
    }

    /// Fixed use against the limits the sandbox really has.
    fn metrics(&self, name: &SandboxName) -> Result<SandboxMetrics> {
        let mut state = self.begin(FakeCall::Metrics(name.clone()))?;
        let limits = running_box(&mut state, name)?.spec.resources;
        Ok(SandboxMetrics {
            cpu_millis: 500,
            memory_used_bytes: 1 << 30,
            memory_limit_bytes: u64::from(limits.memory_mib) << 20,
            memory_host_bytes: 2 << 30,
            disk_used_bytes: Some(3 << 30),
        })
    }

    fn exec(&self, name: &SandboxName, request: &ExecRequest) -> Result<ExecOutput> {
        let mut state = self.begin(FakeCall::Exec(name.clone(), request.clone()))?;
        crate::argv::exec(name, request)?;
        running_box(&mut state, name)?;
        if let Some(root) = state.piped_root.clone().filter(|_| runs_git(request)) {
            drop(state);
            return run_locally(&root, request, &[]);
        }
        Ok(state.exec_results.pop_front().unwrap_or_default())
    }

    fn exec_stream(
        &self,
        name: &SandboxName,
        request: &ExecRequest,
        sink: &mut dyn OutputSink,
    ) -> Result<StreamEnd> {
        let (chunks, code, stays_open) = {
            let mut state = self.begin(FakeCall::ExecStream(name.clone(), request.clone()))?;
            crate::argv::exec_stream(name, request)?;
            running_box(&mut state, name)?;
            (
                std::mem::take(&mut state.stream_chunks),
                state.stream_code,
                state.stream_stays_open,
            )
        };
        let stopped = || {
            self.lock().streams_stopped += 1;
            Ok(StreamEnd::Stopped)
        };
        for (stream, bytes) in chunks {
            if sink.cancelled() || !sink.output(stream, &bytes) {
                return stopped();
            }
        }
        if stays_open {
            // Bounded so a broken test fails rather than hanging for ever.
            let started = Instant::now();
            while started.elapsed() < FAKE_STREAM_LIMIT {
                if sink.cancelled() {
                    return stopped();
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            return Err(EngineError::Timeout {
                operation: "fake stream".to_owned(),
                seconds: FAKE_STREAM_LIMIT.as_secs(),
            });
        }
        Ok(StreamEnd::Exited(code))
    }

    fn exec_with_input(
        &self,
        name: &SandboxName,
        request: &ExecRequest,
        input: &[u8],
    ) -> Result<ExecOutput> {
        let mut state = self.begin(FakeCall::ExecWithInput(name.clone(), request.clone()))?;
        crate::argv::exec_with_input(name, request)?;
        running_box(&mut state, name)?;
        if let Some(root) = state.piped_root.clone().filter(|_| runs_git(request)) {
            drop(state);
            return run_locally(&root, request, input);
        }
        state.inputs.push((
            request.command.clone(),
            String::from_utf8_lossy(input).into_owned(),
        ));
        Ok(state.exec_results.pop_front().unwrap_or_default())
    }

    fn exec_piped(&self, name: &SandboxName, request: &ExecRequest) -> Result<std::process::Child> {
        let mut state = self.begin(FakeCall::ExecPiped(name.clone(), request.clone()))?;
        crate::argv::exec_terminal(name, request, false)?;
        running_box(&mut state, name)?;
        let Some(root) = state.piped_root.clone() else {
            return Err(EngineError::NotSupported {
                operation: "exec_piped",
                reason: "the fake engine was not told where to run piped commands",
            });
        };
        drop(state);
        let (program, args) = request.command.split_first().ok_or(EngineError::InvalidInput {
            field: "command",
            reason: "empty".to_owned(),
        })?;
        std::process::Command::new(program)
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &root)
            .envs(request.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .current_dir(&root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| EngineError::Spawn {
                operation: "fake exec".to_owned(),
                reason: e.kind().to_string(),
            })
    }

    fn exec_detached(&self, name: &SandboxName, request: &ExecRequest) -> Result<String> {
        let mut state = self.begin(FakeCall::ExecDetached(name.clone(), request.clone()))?;
        crate::argv::exec_detached(name, request)?;
        running_box(&mut state, name)?;
        let id = state.calls.len();
        Ok(format!("job_fake_{id}"))
    }

    fn copy_in(&self, name: &SandboxName, host: &Path, guest: &GuestPath) -> Result<()> {
        let mut state = self.begin(FakeCall::CopyIn(
            name.clone(),
            host.to_owned(),
            guest.clone(),
        ))?;
        crate::argv::copy_in(name, host, guest)?;
        let entry = running_box(&mut state, name)?;
        entry.copied.push((host.to_owned(), guest.clone()));
        read_tree(host, guest.as_str(), &mut entry.files);
        Ok(())
    }

    fn logs(&self, name: &SandboxName, options: &LogOptions) -> Result<String> {
        let state = self.begin(FakeCall::Logs(name.clone(), *options))?;
        let entry = state.boxes.get(name).ok_or_else(|| not_found(name))?;
        let skip = options
            .tail
            .map_or(0, |n| entry.log.len().saturating_sub(n as usize));
        Ok(entry.log[skip..].join("\n"))
    }

    fn location(&self) -> Option<EngineLocation> {
        self.lock().location.clone()
    }
}

/// Reads regular files under `host` into `files`, keyed by where they land in
/// the guest. Links are not followed. Only for tests, so it is bounded loosely.
fn read_tree(host: &Path, guest: &str, files: &mut BTreeMap<String, Vec<u8>>) {
    const MAX_FILES: usize = 10_000;
    let Ok(meta) = std::fs::symlink_metadata(host) else {
        return;
    };
    if meta.is_file() {
        if files.len() < MAX_FILES {
            if let Ok(bytes) = std::fs::read(host) {
                files.insert(guest.to_owned(), bytes);
            }
        }
    } else if meta.is_dir() {
        let Ok(entries) = std::fs::read_dir(host) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let below = format!("{}/{name}", guest.trim_end_matches('/'));
            read_tree(&entry.path(), &below, files);
        }
    }
}

/// Whether a command runs Git, for [`FakeEngine::run_piped_in`].
fn runs_git(request: &ExecRequest) -> bool {
    match request.command.first().map(String::as_str) {
        Some("git") => true,
        Some("sh") => request.command.get(2).is_some_and(|script| script.contains("git ")),
        _ => false,
    }
}

fn run_locally(root: &Path, request: &ExecRequest, input: &[u8]) -> Result<ExecOutput> {
    use std::io::Write as _;
    let (program, args) = request.command.split_first().ok_or(EngineError::InvalidInput {
        field: "command",
        reason: "empty".to_owned(),
    })?;
    let spawn_error = |e: std::io::Error| EngineError::Spawn {
        operation: "fake exec".to_owned(),
        reason: e.kind().to_string(),
    };
    let mut child = std::process::Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root)
        .envs(request.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .current_dir(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(spawn_error)?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input);
    }
    let done = child.wait_with_output().map_err(spawn_error)?;
    Ok(ExecOutput {
        code: done.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&done.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&done.stderr).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{NetworkPolicy, Resources};

    fn name() -> SandboxName {
        SandboxName::new("demo").unwrap()
    }

    fn spec() -> CreateSpec {
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
    fn full_lifecycle() {
        let e = FakeEngine::new();
        assert_eq!(e.status(&name()).unwrap(), SandboxStatus::NotFound);
        e.create(&spec()).unwrap();
        assert_eq!(e.status(&name()).unwrap(), SandboxStatus::Running);
        assert_eq!(e.spec(&name()).unwrap(), spec());
        e.stop(&name()).unwrap();
        assert_eq!(e.status(&name()).unwrap(), SandboxStatus::Stopped);
        e.start(&name()).unwrap();
        assert_eq!(e.status(&name()).unwrap(), SandboxStatus::Running);
        e.destroy(&name()).unwrap();
        assert_eq!(e.status(&name()).unwrap(), SandboxStatus::NotFound);
    }

    #[test]
    fn duplicate_create_is_rejected() {
        let e = FakeEngine::new();
        e.create(&spec()).unwrap();
        assert!(matches!(
            e.create(&spec()),
            Err(EngineError::AlreadyExists { .. })
        ));
    }

    #[test]
    fn bad_spec_is_rejected_like_the_real_builder() {
        let e = FakeEngine::new();
        let mut s = spec();
        s.image = "--privileged".to_owned();
        assert!(matches!(
            e.create(&s),
            Err(EngineError::InvalidInput { .. })
        ));
        assert_eq!(e.status(&name()).unwrap(), SandboxStatus::NotFound);
    }

    #[test]
    fn missing_sandbox_is_not_found() {
        let e = FakeEngine::new();
        let n = name();
        let guest = GuestPath::new("/w").unwrap();
        assert!(matches!(e.start(&n), Err(EngineError::NotFound { .. })));
        assert!(matches!(e.stop(&n), Err(EngineError::NotFound { .. })));
        assert!(matches!(e.destroy(&n), Err(EngineError::NotFound { .. })));
        assert!(matches!(
            e.exec(&n, &ExecRequest::new(["true"])),
            Err(EngineError::NotFound { .. })
        ));
        assert!(matches!(
            e.copy_in(&n, Path::new("/x"), &guest),
            Err(EngineError::NotFound { .. })
        ));
        assert!(matches!(
            e.logs(&n, &LogOptions::default()),
            Err(EngineError::NotFound { .. })
        ));
    }

    #[test]
    fn stopped_sandbox_refuses_exec_and_copy() {
        let e = FakeEngine::new();
        e.create(&spec()).unwrap();
        e.stop(&name()).unwrap();
        assert!(matches!(
            e.exec(&name(), &ExecRequest::new(["true"])),
            Err(EngineError::InvalidState { .. })
        ));
        assert!(matches!(
            e.copy_in(&name(), Path::new("/x"), &GuestPath::new("/w").unwrap()),
            Err(EngineError::InvalidState { .. })
        ));
        assert!(matches!(
            e.stop(&name()),
            Err(EngineError::InvalidState { .. })
        ));
    }

    #[test]
    fn start_when_running_is_rejected() {
        let e = FakeEngine::new();
        e.create(&spec()).unwrap();
        assert!(matches!(
            e.start(&name()),
            Err(EngineError::InvalidState { .. })
        ));
    }

    #[derive(Default)]
    struct Collect(Vec<u8>);

    impl OutputSink for Collect {
        fn output(&mut self, _stream: OutputStream, bytes: &[u8]) -> bool {
            self.0.extend_from_slice(bytes);
            true
        }
    }

    /// The fake follows the real adapter's rule: a stopped sandbox is refused.
    #[test]
    fn a_stream_follows_the_same_rules_as_exec() {
        let e = FakeEngine::new();
        let req = ExecRequest::new(["x"]);
        assert!(matches!(
            e.exec_stream(&name(), &req, &mut Collect::default()),
            Err(EngineError::NotFound { .. })
        ));
        e.create(&spec()).unwrap();
        e.push_stream_output(OutputStream::Stdout, b"hello");
        e.set_stream_code(3);
        let mut sink = Collect::default();
        assert_eq!(
            e.exec_stream(&name(), &req, &mut sink).unwrap(),
            StreamEnd::Exited(3)
        );
        assert_eq!(sink.0, b"hello");
        assert!(e
            .exec_stream(&name(), &ExecRequest::new(Vec::<String>::new()), &mut sink)
            .is_err());
        e.stop(&name()).unwrap();
        assert!(matches!(
            e.exec_stream(&name(), &req, &mut sink),
            Err(EngineError::InvalidState { .. })
        ));
    }

    #[test]
    fn exec_returns_scripted_output_in_order_then_default() {
        let e = FakeEngine::new();
        e.create(&spec()).unwrap();
        let first = ExecOutput {
            code: 3,
            stdout: "out".into(),
            stderr: "err".into(),
        };
        e.push_exec_result(first.clone());
        let req = ExecRequest::new(["docker", "info"]);
        assert_eq!(e.exec(&name(), &req).unwrap(), first);
        assert_eq!(e.exec(&name(), &req).unwrap(), ExecOutput::default());
    }

    #[test]
    fn exec_detached_returns_a_job_id_and_needs_a_running_sandbox() {
        let e = FakeEngine::new();
        e.create(&spec()).unwrap();
        let req = ExecRequest::new(["dockerd"]);
        assert!(e.exec_detached(&name(), &req).unwrap().starts_with("job_"));
        e.stop(&name()).unwrap();
        assert!(matches!(
            e.exec_detached(&name(), &req),
            Err(EngineError::InvalidState { .. })
        ));
    }

    #[test]
    fn empty_exec_is_rejected() {
        let e = FakeEngine::new();
        e.create(&spec()).unwrap();
        assert!(e
            .exec(&name(), &ExecRequest::new(Vec::<String>::new()))
            .is_err());
    }

    #[test]
    fn copy_in_is_recorded_and_validates_paths() {
        let e = FakeEngine::new();
        e.create(&spec()).unwrap();
        let guest = GuestPath::new("/work").unwrap();
        e.copy_in(&name(), Path::new("/src"), &guest).unwrap();
        assert_eq!(e.copied(&name()), [(PathBuf::from("/src"), guest.clone())]);
        assert!(e.copy_in(&name(), Path::new("rel"), &guest).is_err());
    }

    /// The real rule, measured: at the default level a refused lookup leaves
    /// nothing behind. Every sandbox made through the builder is at debug.
    #[test]
    fn a_refused_lookup_is_written_only_at_a_level_that_writes_it() {
        let dir = std::env::temp_dir().join(format!("fake-logs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let e = FakeEngine::new();
        e.set_location(EngineLocation {
            binary: PathBuf::from("/nonexistent"),
            home: dir.clone(),
        });
        e.keep_files();
        e.create(&spec()).unwrap();
        let logs = e.logs_dir(&name()).unwrap();
        assert!(logs.is_dir());
        e.deny_lookup(&name(), "blocked.example.net").unwrap();
        let text = std::fs::read_to_string(logs.join("runtime.log")).unwrap();
        assert!(text.ends_with("domain=blocked.example.net\n"), "{text}");
        e.destroy(&name()).unwrap();
        assert!(!logs.exists());
        assert!(matches!(
            e.deny_lookup(&name(), "x.example.net"),
            Err(EngineError::NotFound { .. })
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn logs_honor_tail() {
        let e = FakeEngine::new();
        e.create(&spec()).unwrap();
        for line in ["a", "b", "c"] {
            e.push_log(&name(), line).unwrap();
        }
        assert_eq!(e.logs(&name(), &LogOptions::default()).unwrap(), "a\nb\nc");
        assert_eq!(
            e.logs(&name(), &LogOptions { tail: Some(2) }).unwrap(),
            "b\nc"
        );
        assert_eq!(
            e.logs(&name(), &LogOptions { tail: Some(9) }).unwrap(),
            "a\nb\nc"
        );
    }

    #[test]
    fn fail_next_applies_once_and_is_recorded() {
        let e = FakeEngine::new();
        e.fail_next(EngineError::Timeout {
            operation: "x".into(),
            seconds: 1,
        });
        assert!(matches!(
            e.create(&spec()),
            Err(EngineError::Timeout { .. })
        ));
        assert_eq!(e.calls().len(), 1);
        e.create(&spec()).unwrap();
    }

    #[test]
    fn calls_are_recorded_in_order() {
        let e = FakeEngine::new();
        e.create(&spec()).unwrap();
        e.stop(&name()).unwrap();
        e.destroy(&name()).unwrap();
        assert_eq!(
            e.calls(),
            [
                FakeCall::Create(spec()),
                FakeCall::Stop(name()),
                FakeCall::Destroy(name()),
            ]
        );
    }

    #[test]
    fn usable_as_trait_object_across_threads() {
        let e: std::sync::Arc<dyn Engine> = std::sync::Arc::new(FakeEngine::new());
        let e2 = e.clone();
        std::thread::spawn(move || e2.create(&spec()).unwrap())
            .join()
            .unwrap();
        assert_eq!(e.status(&name()).unwrap(), SandboxStatus::Running);
    }
}
