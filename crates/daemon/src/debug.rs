//! Looking inside a running sandbox: its logs, and the way to a terminal.
//!
//! Neither may change what is running, so both refuse a sandbox that is not
//! running. That check matters more than it looks: the engine's own `exec`
//! boots a stopped sandbox for the length of the command, without the secrets
//! and rules a proper start would apply.
//!
//! Logs come from inside the sandbox, so they are filtered here before they
//! reach any client. Secret values are not masked: a secret assigned to a service
//! does reach its sandbox, so whatever that service prints can show the value.

use std::time::Duration;

use engine::{EngineLocation, ExecRequest, GuestPath, OutputSink, StreamEnd};
use proto::text::{self, TerminalFilter};

use crate::mask::{Masker, StreamMasker};
use proto::{
    check_service_name, ApiError, LogsArgs, LogsReport, OutputStream, SandboxState, TerminalArgs,
    TerminalReport, MAX_LOG_TAIL,
};

use crate::run::{status_of, WORK_DIR};
use crate::{audit, bad, engine_error, internal, not_found, unsupported, wrong_state, Progress};
use crate::{Project, Service};

/// How long a log read that is not following may take.
const TAIL_LIMIT: Duration = Duration::from_secs(60);
/// The most output a log read that is not following may send, so a sandbox
/// cannot fill the memory of a client that keeps what it is given.
const MAX_TAIL_BYTES: u64 = 64 * 1024 * 1024;

fn check_service(service: Option<&str>) -> Result<(), ApiError> {
    match service {
        Some(name) => check_service_name(name).map_err(bad),
        None => Ok(()),
    }
}

pub(crate) fn check_logs_args(args: &LogsArgs) -> Result<(), ApiError> {
    check_service(args.service.as_deref())?;
    if args.tail > MAX_LOG_TAIL {
        return Err(bad(format!(
            "the most lines that can be asked for is {MAX_LOG_TAIL}"
        )));
    }
    Ok(())
}

pub(crate) fn check_terminal_args(args: &TerminalArgs) -> Result<(), ApiError> {
    check_service(args.service.as_deref())
}

/// Refuses anything but a running sandbox, saying how to start it.
fn require_running(service: &Service, project: &Project) -> Result<(), ApiError> {
    let how = || {
        format!(
            "Nothing was started. To start it, run: {} up {}",
            brand::CLI_BIN,
            text::escape_path(&project.root)
        )
    };
    match status_of(service, project)? {
        SandboxState::Running => Ok(()),
        SandboxState::Stopped => Err(wrong_state(format!(
            "the sandbox for this project is stopped. {}",
            how()
        ))),
        SandboxState::None => Err(not_found(format!(
            "this project has no sandbox. {}",
            how()
        ))),
        SandboxState::Unknown => Err(wrong_state(
            "the sandbox is in a state this version does not understand, so nothing was run in it.",
        )),
    }
}

fn workdir() -> Result<GuestPath, ApiError> {
    GuestPath::new(WORK_DIR).map_err(|e| internal(e.to_string()))
}

fn log_command(args: &LogsArgs) -> Vec<String> {
    let mut command: Vec<String> = ["docker", "compose", "logs", "--no-color", "--tail"]
        .map(str::to_owned)
        .into();
    command.push(args.tail.to_string());
    if args.follow {
        command.push("--follow".to_owned());
    }
    if let Some(service) = &args.service {
        command.push(service.clone());
    }
    command
}

/// Filters output on its way from the engine to the client.
struct FilteredOutput<'a> {
    progress: &'a mut dyn Progress,
    stdout: TerminalFilter,
    stderr: TerminalFilter,
    /// Secret values are replaced after control codes are removed, so an
    /// escape sequence cannot split a value and hide it from the search.
    masker: &'a Masker,
    masked_out: StreamMasker,
    masked_err: StreamMasker,
    remaining: Option<u64>,
    truncated: bool,
}

impl FilteredOutput<'_> {
    fn finish(&mut self) {
        let mut out = self.masked_out.push(self.masker, &self.stdout.finish());
        out.push_str(&self.masked_out.finish(self.masker));
        let mut err = self.masked_err.push(self.masker, &self.stderr.finish());
        err.push_str(&self.masked_err.finish(self.masker));
        for (stream, text) in [(OutputStream::Stdout, out), (OutputStream::Stderr, err)] {
            if !text.is_empty() {
                self.progress.output(stream, &text);
            }
        }
    }
}

impl OutputSink for FilteredOutput<'_> {
    fn output(&mut self, stream: engine::OutputStream, bytes: &[u8]) -> bool {
        if let Some(remaining) = &mut self.remaining {
            let size = bytes.len() as u64;
            if size > *remaining {
                self.truncated = true;
                return false;
            }
            *remaining -= size;
        }
        let (stream, text) = match stream {
            engine::OutputStream::Stdout => {
                let text = self.stdout.push(bytes);
                (OutputStream::Stdout, self.masked_out.push(self.masker, &text))
            }
            engine::OutputStream::Stderr => {
                let text = self.stderr.push(bytes);
                (OutputStream::Stderr, self.masked_err.push(self.masker, &text))
            }
        };
        text.is_empty() || self.progress.output(stream, &text)
    }

    fn cancelled(&self) -> bool {
        self.progress.cancelled()
    }
}

pub fn logs(
    service: &Service,
    project: &Project,
    args: &LogsArgs,
    progress: &mut dyn Progress,
) -> Result<LogsReport, ApiError> {
    require_running(service, project)?;
    let mut request = ExecRequest::new(log_command(args));
    request.workdir = Some(workdir()?);
    request.timeout = (!args.follow).then_some(TAIL_LIMIT);
    let masker = Masker::for_project(service, project)?;
    let mut sink = FilteredOutput {
        progress,
        stdout: TerminalFilter::new(),
        stderr: TerminalFilter::new(),
        masker: &masker,
        masked_out: StreamMasker::default(),
        masked_err: StreamMasker::default(),
        remaining: (!args.follow).then_some(MAX_TAIL_BYTES),
        truncated: false,
    };
    let result = service
        .deps()
        .engine()
        .exec_stream(&project.sandbox, &request, &mut sink);
    sink.finish();
    let end = result.map_err(|e| engine_error(e.to_string()))?;
    Ok(LogsReport {
        code: match end {
            StreamEnd::Exited(code) => Some(code),
            StreamEnd::Stopped => None,
        },
        truncated: sink.truncated,
    })
}

/// A path the client will run has to arrive exactly as it is, and the rule for
/// text sent over the socket would change one with a control character in it.
fn exact_path(path: &std::path::Path) -> Result<String, ApiError> {
    let text = path
        .to_str()
        .ok_or_else(|| internal("the engine's path cannot be sent to a client"))?;
    if text::escape(text) != text {
        return Err(internal("the engine's path cannot be sent to a client"));
    }
    Ok(text.to_owned())
}

/// Tells a client how to run the engine on the person's own terminal.
///
/// The daemon has no terminal to relay, and relaying one is what an SSH server
/// is for. The client runs the engine itself, with a cleared environment, so
/// this gives it only the two paths and the sandbox's name. A process of the
/// same user could already find all three in the state folder.
pub fn terminal(
    service: &Service,
    project: &Project,
    args: &TerminalArgs,
) -> Result<TerminalReport, ApiError> {
    require_running(service, project)?;
    let Some(EngineLocation { binary, home }) = service.deps().engine().location() else {
        return Err(unsupported(
            "this engine cannot be run from a client's terminal",
        ));
    };
    let report = TerminalReport {
        sandbox: project.sandbox.as_str().to_owned(),
        engine_binary: exact_path(&binary)?,
        engine_home: exact_path(&home)?,
        workdir: workdir()?.as_str().to_owned(),
    };
    let mut entry = audit::entry(service.deps().now_unix(), audit::Action::TerminalOpened)
        .project(project.id.as_str());
    if let Some(name) = &args.service {
        entry = entry.subject(name);
    }
    service.record(entry);
    Ok(report)
}
