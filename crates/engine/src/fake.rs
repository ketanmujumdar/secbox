//! An in-memory engine for tests. It follows the same lifecycle rules a real
//! engine should, and records every call.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::error::{EngineError, Result};
use crate::types::{
    CreateSpec, ExecOutput, ExecRequest, GuestPath, LogOptions, SandboxName, SandboxStatus,
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
    inputs: Vec<(Vec<String>, String)>,
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
        state
            .boxes
            .remove(name)
            .map(|_| ())
            .ok_or_else(|| not_found(name))
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
