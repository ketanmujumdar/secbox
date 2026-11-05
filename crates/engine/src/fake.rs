//! An in-memory engine for tests. It follows the same lifecycle rules a real
//! engine should, and records every call.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::error::{EngineError, Result};
use crate::types::{
    CreateSpec, EngineLocation, ExecOutput, ExecRequest, GuestPath, LogOptions, OutputSink,
    OutputStream, SandboxName, SandboxStatus, StreamEnd,
};
use crate::Engine;

/// A call made to a [`FakeEngine`], in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeCall {
    Create(CreateSpec),
    Start(SandboxName),
    Stop(SandboxName),
    Destroy(SandboxName),
    Status(SandboxName),
    Exec(SandboxName, ExecRequest),
    ExecStream(SandboxName, ExecRequest),
    /// The input is deliberately not recorded here, so that a secret cannot
    /// be printed by a failing test. Read it with [`FakeEngine::inputs`].
    ExecWithInput(SandboxName, ExecRequest),
    ExecDetached(SandboxName, ExecRequest),
    CopyIn(SandboxName, PathBuf, GuestPath),
    Logs(SandboxName, LogOptions),
}

#[derive(Debug)]
struct Box_ {
    running: bool,
    spec: CreateSpec,
    copied: Vec<(PathBuf, GuestPath)>,
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
    inputs: Vec<(Vec<String>, String)>,
    keeps_files: bool,
    lookups_denied: u32,
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

    fn exec(&self, name: &SandboxName, request: &ExecRequest) -> Result<ExecOutput> {
        let mut state = self.begin(FakeCall::Exec(name.clone(), request.clone()))?;
        crate::argv::exec(name, request)?;
        running_box(&mut state, name)?;
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
        state.inputs.push((
            request.command.clone(),
            String::from_utf8_lossy(input).into_owned(),
        ));
        Ok(state.exec_results.pop_front().unwrap_or_default())
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
        running_box(&mut state, name)?
            .copied
            .push((host.to_owned(), guest.clone()));
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
