//! The command line. The binary is a thin wrapper around [`run`], which takes
//! its secret store, prompt and host facts as [`Deps`], so tests can run every
//! command in-process with a memory store. There is no flag or variable that
//! swaps the store in the real binary.

mod check;
pub mod client;
mod doctor;
mod editor;
pub mod host;
mod net;
mod resources;
mod secret_cmd;
pub mod state;
mod ui;
mod uninstall;
mod vm;

use std::ffi::OsString;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use secrets::{KeyringStore, ProjectId, SecretStore, SecretValue};

pub use client::{CallError, DaemonClient, NoDaemon, SocketClient};
pub use host::{HostInfo, Hypervisor};
pub use ui::esc;

pub mod exit {
    pub const OK: i32 = 0;
    /// A check failed, or something was refused or aborted.
    pub const FAILED: i32 = 1;
    /// The command line was wrong.
    pub const USAGE: i32 = 2;
    /// `check --strict` found Critical or High findings.
    pub const FINDINGS: i32 = 3;
    /// `net check` explained a denied request.
    pub const DENIED: i32 = 4;
    /// The command exists but is not built yet.
    pub const NOT_BUILT: i32 = 69;
}

/// Everything a command needs from the outside world.
pub struct Deps<'a> {
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
    pub input: &'a mut dyn BufRead,
    pub color: bool,
    pub cwd: PathBuf,
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
    pub store: &'a dyn Fn(&ProjectId) -> Box<dyn SecretStore>,
    pub prompt: &'a dyn Fn(&str) -> io::Result<SecretValue>,
    pub host: &'a dyn Fn(&Path) -> HostInfo,
    /// Where requests go. The command line decides nothing on its own.
    pub daemon: &'a dyn DaemonClient,
}

/// Prints a line to stdout. A closed pipe is not an error worth reporting.
macro_rules! say {
    ($d:expr) => { let _ = writeln!($d.out); };
    ($d:expr, $($arg:tt)*) => { let _ = writeln!($d.out, $($arg)*); };
}
/// Prints a line to stderr.
macro_rules! tell {
    ($d:expr, $($arg:tt)*) => { let _ = writeln!($d.err, $($arg)*); };
}
pub(crate) use {say, tell};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check whether this machine can run sandboxes
    Doctor,
    /// Check a project's files and print what they ask for
    Check(check::CheckArgs),
    /// Run the editor safety checks, then open the project in an editor
    Open(editor::OpenArgs),
    /// Turn the editor safety checks on and off, at user level
    Checks(editor::ChecksArgs),
    /// Show requested and resolved RAM, CPU and disk limits
    Resources(resources::ResourcesArgs),
    /// Manage per-project secrets in the OS keychain
    Secrets(secret_cmd::SecretsArgs),
    /// Explain network policy decisions
    Net(net::NetArgs),
    /// Remove the state folder (needs --purge)
    Uninstall(uninstall::UninstallArgs),
    /// Start a project in a sandbox
    Up(vm::UpArgsCli),
    /// Stop a project's sandbox, or destroy it
    Down(vm::DownArgsCli),
    /// Show a project's sandbox
    Status(vm::StatusArgsCli),
    /// Not built yet: review changes made in a VM
    Diff(NotBuiltArgs),
    /// Not built yet: export reviewed changes through Git
    Export(NotBuiltArgs),
}

/// These commands accept anything, so that a flag from the docs gets the real
/// answer rather than a parse error.
#[derive(Args)]
struct NotBuiltArgs {
    #[arg(hide = true, trailing_var_arg = true, allow_hyphen_values = true)]
    ignored: Vec<OsString>,
}

fn command() -> clap::Command {
    Cli::command()
        .name(brand::CLI_BIN)
        .bin_name(brand::CLI_BIN)
        .version(env!("CARGO_PKG_VERSION"))
        .about("Run a project and its AI agent in a disposable microVM (early build)")
        .after_help(format!(
            "Status: {}\n\nExit codes: 0 ok, 1 failed or refused, 2 bad usage, 3 strict findings, \
             4 request denied, {} not built yet.",
            ui::not_protecting(),
            exit::NOT_BUILT
        ))
        .help_template("{about}\n\nUsage: {usage}\n\n{all-args}{after-help}\n")
}

/// Parses the arguments and runs one command. Returns the exit code.
pub fn run<I, T>(args: I, deps: &mut Deps) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let mut matches = match command().try_get_matches_from(args) {
        Ok(matches) => matches,
        Err(error) => {
            let text = error.render().to_string();
            return if error.use_stderr() {
                tell!(deps, "{}", text.trim_end());
                exit::USAGE
            } else {
                say!(deps, "{}", text.trim_end());
                exit::OK
            };
        }
    };
    let cli = match Cli::from_arg_matches_mut(&mut matches) {
        Ok(cli) => cli,
        Err(error) => {
            tell!(deps, "{}", error.render().to_string().trim_end());
            return exit::USAGE;
        }
    };
    match cli.command {
        Command::Doctor => doctor::run(deps),
        Command::Check(args) => check::run(&args, deps),
        Command::Open(args) => editor::open(&args, deps),
        Command::Checks(args) => editor::checks(&args, deps),
        Command::Resources(args) => resources::run(&args, deps),
        Command::Secrets(args) => secret_cmd::run(args, deps),
        Command::Net(args) => net::run(&args, deps),
        Command::Uninstall(args) => uninstall::run(&args, deps),
        Command::Up(args) => vm::up(&args, deps),
        Command::Down(args) => vm::down(&args, deps),
        Command::Status(args) => vm::status(&args, deps),
        Command::Diff(_) => not_built("diff", deps),
        Command::Export(_) => not_built("export", deps),
    }
}

fn not_built(name: &str, deps: &mut Deps) -> i32 {
    let bad = ui::paint(deps.color, ui::Tone::Bad, "not built yet");
    tell!(deps, "{} {name}: {bad}", brand::CLI_BIN);
    tell!(deps, "Nothing was started, stopped or changed.");
    tell!(deps, "{}", ui::not_protecting());
    tell!(
        deps,
        "Do not run untrusted code on the assumption that this tool contains it."
    );
    exit::NOT_BUILT
}

/// Resolves a project path argument to a real directory.
pub(crate) fn project_dir(path: Option<&Path>, deps: &Deps) -> Result<PathBuf, String> {
    let raw = match path {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => deps.cwd.join(path),
        None => deps.cwd.clone(),
    };
    let real = std::fs::canonicalize(&raw)
        .map_err(|e| format!("cannot open {}: {}", ui::esc_path(&raw), e.kind()))?;
    if !real.is_dir() {
        return Err(format!("{} is not a directory", ui::esc_path(&real)));
    }
    Ok(real)
}

/// Color is for terminals only. Any non-empty `NO_COLOR` turns it off.
fn color_enabled(terminal: bool, no_color: Option<OsString>, term: Option<OsString>) -> bool {
    terminal && no_color.is_none_or(|v| v.is_empty()) && term.is_none_or(|v| v != "dumb")
}

/// The entry point for the real binary: real streams, keychain and prompt.
pub fn real_main() -> i32 {
    let stdout = io::stdout();
    let stderr = io::stderr();
    let stdin = io::stdin();
    let color = color_enabled(
        stdout.is_terminal(),
        std::env::var_os("NO_COLOR"),
        std::env::var_os("TERM"),
    );
    let env = |key: &str| std::env::var_os(key);
    let store =
        |project: &ProjectId| Box::new(KeyringStore::new(project.clone())) as Box<dyn SecretStore>;
    let prompt = |label: &str| rpassword::prompt_password(label).map(SecretValue::new);
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => {
            let _ = writeln!(io::stderr(), "cannot read the current folder: {}", e.kind());
            return exit::FAILED;
        }
    };
    // Where the daemon's socket is. A state folder that cannot even be
    // located is reported once here, rather than by each command.
    let located = state::StatePaths::locate(&env);
    let socket_client;
    let no_daemon;
    let daemon: &dyn DaemonClient = match &located {
        Ok(paths) => {
            socket_client = SocketClient::new(&paths.state);
            &socket_client
        }
        Err(reason) => {
            no_daemon = NoDaemon {
                reason: reason.clone(),
            };
            &no_daemon
        }
    };
    let code = run(
        std::env::args_os(),
        &mut Deps {
            out: &mut stdout.lock(),
            err: &mut stderr.lock(),
            input: &mut stdin.lock(),
            color,
            cwd,
            env: &env,
            store: &store,
            prompt: &prompt,
            host: &host::probe,
            daemon,
        },
    );
    let _ = io::stdout().flush();
    code
}

#[cfg(test)]
mod tests {
    use super::color_enabled;

    #[test]
    fn color_needs_a_terminal_and_honors_no_color() {
        let on = |v: &str| Some(v.into());
        assert!(color_enabled(true, None, on("xterm")));
        assert!(color_enabled(true, on(""), None));
        assert!(!color_enabled(false, None, on("xterm")));
        assert!(!color_enabled(true, on("1"), on("xterm")));
        assert!(!color_enabled(true, None, on("dumb")));
    }
}
