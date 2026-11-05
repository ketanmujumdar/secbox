//! `logs`, `shell` and `exec`: looking inside a running sandbox.
//!
//! Logs come through the daemon, already filtered, and are filtered again here
//! because this is the last thing they pass through before a terminal.
//!
//! A shell has to be on the person's real terminal. There is no SSH server and
//! no open port in the sandbox, so this asks the daemon, which checks that the
//! sandbox is running, and then runs the engine directly. The environment here
//! may not name the engine, and nothing searches the path or the current
//! folder for it: the daemon's answer is the only source.

use std::io::{IsTerminal, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Args;
use engine::{argv, ExecRequest, GuestPath, SandboxName};
use proto::text::TerminalFilter;
use proto::{
    check_service_name, LogsArgs, OutputStream, Reply, Request, TerminalArgs, TerminalReport,
    MAX_LOG_TAIL,
};

use crate::client::CallError;
use crate::tui::SavedSettings;
use crate::ui::esc;
use crate::{exit, project_dir, tell, Deps};

#[derive(Args)]
pub struct LogsArgsCli {
    /// One service [default: all of them]
    service: Option<String>,
    /// Project folder [default: the current folder]
    #[arg(long, value_name = "PATH")]
    project: Option<PathBuf>,
    /// Keep showing new lines until Ctrl-C
    #[arg(short, long)]
    follow: bool,
    /// How many recent lines to show from each service
    #[arg(long, value_name = "N", default_value_t = 200)]
    tail: u32,
}

#[derive(Args)]
pub struct ShellArgsCli {
    /// Open the shell in this service's container [default: in the sandbox itself]
    service: Option<String>,
    /// Project folder [default: the current folder]
    #[arg(long, value_name = "PATH")]
    project: Option<PathBuf>,
}

#[derive(Args)]
pub struct ExecArgsCli {
    /// Run the command in this service's container [default: in the sandbox itself]
    #[arg(long, value_name = "SERVICE")]
    service: Option<String>,
    /// Project folder [default: the current folder]
    #[arg(long, value_name = "PATH")]
    project: Option<PathBuf>,
    /// The command, after --
    #[arg(last = true, required = true, value_name = "CMD")]
    command: Vec<String>,
}

fn valid_service(name: Option<&str>, command: &str, deps: &mut Deps) -> bool {
    match name.map(check_service_name) {
        Some(Err(reason)) => {
            tell!(deps, "{command}: {reason}.");
            false
        }
        _ => true,
    }
}

fn root_text(path: Option<&Path>, command: &str, deps: &mut Deps) -> Option<String> {
    match project_dir(path, deps) {
        Ok(root) => Some(root.to_string_lossy().into_owned()),
        Err(reason) => {
            tell!(deps, "{command}: {reason}");
            None
        }
    }
}

pub fn logs(args: &LogsArgsCli, deps: &mut Deps) -> i32 {
    if !valid_service(args.service.as_deref(), "logs", deps) {
        return exit::USAGE;
    }
    if args.tail > MAX_LOG_TAIL {
        tell!(deps, "logs: the most lines that can be asked for is {MAX_LOG_TAIL}.");
        return exit::USAGE;
    }
    let Some(project) = root_text(args.project.as_deref(), "logs", deps) else {
        return exit::FAILED;
    };
    let request = Request::Logs(LogsArgs {
        project,
        service: args.service.clone(),
        tail: args.tail,
        follow: args.follow,
    });

    let client = deps.daemon;
    let mut stdout = TerminalFilter::new();
    let mut stderr = TerminalFilter::new();
    // No time limit: the daemon limits a plain read, and following has no end.
    // Ctrl-C ends this process, whose closed connection stops the log command.
    // Nothing here changes the terminal, so there is nothing to put back.
    let result = client.stream(
        request,
        &mut |stream, text| {
            let (filter, writer): (_, &mut dyn Write) = match stream {
                OutputStream::Stdout => (&mut stdout, &mut *deps.out),
                OutputStream::Stderr => (&mut stderr, &mut *deps.err),
            };
            let clean = filter.push(text.as_bytes());
            writer.write_all(clean.as_bytes()).is_ok() && writer.flush().is_ok()
        },
        None,
    );
    let _ = deps.out.write_all(stdout.finish().as_bytes());
    let _ = deps.err.write_all(stderr.finish().as_bytes());
    let _ = deps.out.flush();

    match result {
        Ok(Reply::Logs(report)) => {
            if report.truncated {
                tell!(deps, "logs: this was cut off at the daemon's limit. Ask for fewer lines with --tail.");
            }
            match report.code {
                Some(0) | None => exit::OK,
                Some(code) => {
                    tell!(deps, "logs: the log command inside the sandbox exited with {code}.");
                    exit::FAILED
                }
            }
        }
        Ok(_) => {
            tell!(deps, "logs: the daemon answered something else.");
            exit::FAILED
        }
        // Whoever was reading has gone, as with a pipe into `head`.
        Err(CallError::Stopped) => exit::OK,
        Err(error) => {
            tell!(deps, "logs: {}", esc(&error.message()));
            exit::FAILED
        }
    }
}

pub fn shell(args: &ShellArgsCli, deps: &mut Deps) -> i32 {
    let tty = on_a_terminal();
    let command = shell_command(args.service.as_deref(), tty);
    open_terminal(
        "shell",
        args.service.as_deref(),
        args.project.as_deref(),
        &command,
        tty,
        deps,
    )
}

pub fn exec(args: &ExecArgsCli, deps: &mut Deps) -> i32 {
    let tty = on_a_terminal();
    let command = exec_command(args.service.as_deref(), &args.command, tty);
    open_terminal(
        "exec",
        args.service.as_deref(),
        args.project.as_deref(),
        &command,
        tty,
        deps,
    )
}

fn on_a_terminal() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// What runs inside the sandbox for `shell`.
fn shell_command(service: Option<&str>, tty: bool) -> Vec<String> {
    match service {
        Some(service) => compose_exec(service, &["sh".to_owned()], tty),
        None => vec!["sh".to_owned()],
    }
}

/// What runs inside the sandbox for `exec`.
fn exec_command(service: Option<&str>, command: &[String], tty: bool) -> Vec<String> {
    match service {
        Some(service) => compose_exec(service, command, tty),
        None => command.to_vec(),
    }
}

/// Compose asks for a terminal by default, and fails if there is none.
fn compose_exec(service: &str, command: &[String], tty: bool) -> Vec<String> {
    let mut all: Vec<String> = ["docker", "compose", "exec"].map(str::to_owned).into();
    if !tty {
        all.push("-T".to_owned());
    }
    all.push(service.to_owned());
    all.extend(command.iter().cloned());
    all
}

/// The engine's command, built from the daemon's answer. That answer is
/// checked rather than assumed, because this is about to run what it names.
fn engine_command(
    report: &TerminalReport,
    command: &[String],
    tty: bool,
    term: Option<&str>,
) -> Result<Command, String> {
    let binary = Path::new(&report.engine_binary);
    let home = Path::new(&report.engine_home);
    if !binary.is_absolute() || !home.is_absolute() {
        return Err("the daemon gave a path that is not absolute, so nothing was run".to_owned());
    }
    if !binary.is_file() {
        return Err(format!(
            "the engine is not at {}, so nothing was run",
            esc(&report.engine_binary)
        ));
    }
    let name = SandboxName::new(report.sandbox.as_str()).map_err(|e| esc(&e.to_string()))?;
    let mut request = ExecRequest::new(command.iter().cloned());
    request.workdir =
        Some(GuestPath::new(report.workdir.as_str()).map_err(|e| esc(&e.to_string()))?);
    let args = argv::exec_terminal(&name, &request, tty).map_err(|e| esc(&e.to_string()))?;

    let mut engine = Command::new(binary);
    engine
        .args(args)
        .env_clear()
        .env(argv::HOME_VAR, home)
        // Not the project's folder, which is untrusted and is none of its business.
        .current_dir("/");
    // A shell without this cannot draw properly. Nothing else crosses over.
    if let Some(term) = term.filter(|term| is_plain_term(term)) {
        engine.env("TERM", term);
    }
    Ok(engine)
}

fn is_plain_term(term: &str) -> bool {
    !term.is_empty()
        && term.len() <= 64
        && term
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'))
}

fn notice(deps: &mut Deps) {
    tell!(
        deps,
        "{}: this runs inside the sandbox. Anything you paste there is inside it too.",
        brand::CLI_BIN
    );
    tell!(
        deps,
        "Terminal control codes can't be filtered in an interactive session, so what runs there can affect your terminal."
    );
}

fn open_terminal(
    name: &str,
    service: Option<&str>,
    project: Option<&Path>,
    command: &[String],
    tty: bool,
    deps: &mut Deps,
) -> i32 {
    if !valid_service(service, name, deps) {
        return exit::USAGE;
    }
    let Some(project) = root_text(project, name, deps) else {
        return exit::FAILED;
    };
    // The engine's own `exec` boots a stopped sandbox, so the daemon checks the
    // state first and refuses. Nothing below runs unless it said yes.
    let report = match deps.daemon.call(
        Request::Terminal(TerminalArgs {
            project,
            service: service.map(str::to_owned),
        }),
        &mut |_, _| {},
    ) {
        Ok(Reply::Terminal(report)) => report,
        Ok(_) => {
            tell!(deps, "{name}: the daemon answered something else.");
            return exit::FAILED;
        }
        Err(error) => {
            tell!(deps, "{name}: {}", esc(&error.message()));
            return exit::FAILED;
        }
    };
    let term = (deps.env)("TERM").map(|term| term.to_string_lossy().into_owned());
    let mut engine = match engine_command(&report, command, tty, term.as_deref()) {
        Ok(engine) => engine,
        Err(reason) => {
            tell!(deps, "{name}: {reason}");
            return exit::FAILED;
        }
    };
    notice(deps);
    let _ = deps.err.flush();
    let _ = deps.out.flush();

    // If the engine dies without restoring the terminal, this does.
    let saved = if tty { SavedSettings::read() } else { None };
    let status = engine.status();
    drop(saved);
    match status {
        Ok(status) => status
            .code()
            .or_else(|| status.signal().map(|signal| 128 + signal))
            .unwrap_or(exit::FAILED),
        Err(error) => {
            tell!(deps, "{name}: cannot run the engine: {}", error.kind());
            exit::FAILED
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn a_shell_runs_in_the_sandbox_or_through_compose() {
        assert_eq!(shell_command(None, true), ["sh"]);
        assert_eq!(
            shell_command(Some("web"), true),
            ["docker", "compose", "exec", "web", "sh"]
        );
    }

    #[test]
    fn without_a_terminal_compose_is_told_not_to_ask_for_one() {
        assert_eq!(
            shell_command(Some("web"), false),
            ["docker", "compose", "exec", "-T", "web", "sh"]
        );
        assert_eq!(
            exec_command(Some("web"), &strings(&["ls", "-l"]), false),
            ["docker", "compose", "exec", "-T", "web", "ls", "-l"]
        );
    }

    #[test]
    fn a_command_with_no_service_runs_as_it_was_given() {
        assert_eq!(
            exec_command(None, &strings(&["sh", "-c", "a;b"]), true),
            ["sh", "-c", "a;b"]
        );
    }

    fn report() -> TerminalReport {
        TerminalReport {
            sandbox: "demo".to_owned(),
            engine_binary: "/bin/sh".to_owned(),
            engine_home: "/tmp/h".to_owned(),
            workdir: "/work".to_owned(),
        }
    }

    #[test]
    fn the_engine_gets_a_cleared_environment_and_a_fixed_argument_list() {
        let engine = engine_command(&report(), &strings(&["sh"]), true, Some("xterm-256color"))
            .unwrap();
        assert_eq!(engine.get_program(), "/bin/sh");
        let args: Vec<_> = engine.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(args, ["exec", "--tty", "--workdir", "/work", "demo", "--", "sh"]);
        let mut env: Vec<_> = engine
            .get_envs()
            .map(|(k, v)| (k.to_str().unwrap(), v.and_then(|v| v.to_str())))
            .collect();
        env.sort_unstable();
        assert_eq!(
            env,
            [
                (argv::HOME_VAR, Some("/tmp/h")),
                ("TERM", Some("xterm-256color"))
            ]
        );
        assert_eq!(engine.get_current_dir(), Some(Path::new("/")));
    }

    /// `Command` searches the path for a bare name, so one must never get here.
    #[test]
    fn a_path_that_is_not_absolute_is_never_run() {
        for binary in ["msb", "./msb", "bin/msb", ""] {
            let mut bad = report();
            bad.engine_binary = binary.to_owned();
            assert!(engine_command(&bad, &strings(&["sh"]), true, None).is_err(), "{binary:?}");
        }
        let mut bad = report();
        bad.engine_home = "rel".to_owned();
        assert!(engine_command(&bad, &strings(&["sh"]), true, None).is_err());
    }

    #[test]
    fn a_missing_engine_or_a_bad_sandbox_name_is_refused() {
        let mut bad = report();
        bad.engine_binary = "/nonexistent/engine".to_owned();
        assert!(engine_command(&bad, &strings(&["sh"]), true, None).is_err());
        let mut bad = report();
        bad.sandbox = "--force".to_owned();
        assert!(engine_command(&bad, &strings(&["sh"]), true, None).is_err());
        let mut bad = report();
        bad.workdir = "work".to_owned();
        assert!(engine_command(&bad, &strings(&["sh"]), true, None).is_err());
    }

    #[test]
    fn a_strange_terminal_name_does_not_cross_over() {
        for term in ["", "a b", "x\u{1b}[0m", "$(id)", &"x".repeat(65)] {
            let engine = engine_command(&report(), &strings(&["sh"]), true, Some(term)).unwrap();
            assert!(engine.get_envs().all(|(k, _)| k != "TERM"), "{term:?}");
        }
    }
}
