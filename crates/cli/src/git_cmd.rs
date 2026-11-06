//! `git`: Git for a project imported from a remote.
//!
//! The project is a real clone inside the sandbox, so everything but talking to
//! the remote works there with plain Git (`shell` or `exec`). Pull and push go
//! through the daemon's relay, which uses the person's own SSH key on this
//! machine and checks each push. See `docs/GIT.md`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use clap::{Args, Subcommand};
use proto::{GitArgs, GitIdentity, GitOp, GitReport, Reply, Request};

use crate::ui::esc;
use crate::{exit, project_dir, say, tell, Deps};

#[derive(Args)]
pub struct GitArgsCli {
    #[command(subcommand)]
    op: GitOpCli,
    /// Project folder or SSH remote [default: the current folder]
    #[arg(long, value_name = "PATH", global = true)]
    project: Option<PathBuf>,
}

#[derive(Subcommand)]
enum GitOpCli {
    /// Show the branch and what changed in the sandbox
    Status,
    /// Fetch and merge the current branch from the remote, with your SSH key on this machine
    Pull,
    /// Commit what changed in the sandbox, under your own Git name
    Commit {
        /// The commit message
        #[arg(short, long)]
        message: String,
        /// Only these paths [default: everything that changed]
        paths: Vec<String>,
    },
    /// Push the current branch to the remote, with your SSH key on this machine. Never main or master
    Push,
    /// Run any Git command in the sandbox's clone, such as: run stash, run switch -c fix, run log --oneline.
    /// fetch, pull and push use your SSH key on this machine through the relay; nothing else reaches the remote
    Run {
        /// The command and its arguments, without the leading git
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true, value_name = "GIT_ARGS")]
        args: Vec<String>,
    },
}

pub fn run(args: &GitArgsCli, deps: &mut Deps) -> i32 {
    let project = match project_dir(args.project.as_deref(), deps) {
        Ok(project) => project,
        Err(reason) => {
            tell!(deps, "git: {reason}");
            return exit::FAILED;
        }
    };
    let op = match &args.op {
        GitOpCli::Status => GitOp::Status,
        GitOpCli::Pull => GitOp::Pull,
        GitOpCli::Push => GitOp::Push,
        GitOpCli::Commit { message, paths } => GitOp::Commit {
            message: message.clone(),
            paths: paths.clone(),
        },
        GitOpCli::Run { args } => GitOp::Run { args: args.clone() },
    };
    match call(&project, op, deps) {
        Ok(report) => {
            print_report(&report, deps);
            if report.ok {
                exit::OK
            } else {
                exit::FAILED
            }
        }
        Err(message) => {
            tell!(deps, "git: {message}");
            exit::FAILED
        }
    }
}

/// Sends one Git request with the person's identity and agent. Shared with
/// the MCP tools, so an agent's pull and push are exactly the person's.
pub(crate) fn call(project: &Path, op: GitOp, deps: &Deps) -> Result<GitReport, String> {
    let request = Request::Git(GitArgs {
        project: project.to_string_lossy().into_owned(),
        action: op,
        identity: host_identity(deps),
        ssh_auth_sock: agent_socket(deps),
    });
    match deps.daemon.call(request, &mut |_, _| {}) {
        Ok(Reply::Git(report)) => Ok(report),
        Ok(_) => Err("the daemon answered something else".to_owned()),
        Err(error) => Err(esc(&error.message())),
    }
}

/// The agent this shell uses, which may be newer than the daemon's. A path,
/// not a key; the daemon checks it belongs to this user.
pub(crate) fn agent_socket(deps: &Deps) -> Option<String> {
    (deps.env)("SSH_AUTH_SOCK")
        .map(|path| path.to_string_lossy().into_owned())
        .filter(|path| !path.is_empty())
}

/// The person's name and email from their own global Git settings, so
/// commits made in the sandbox are theirs. Only the global file is read:
/// never the settings of whatever repository the current folder is in.
fn host_identity(deps: &Deps) -> Option<GitIdentity> {
    if let (Some(name), Some(email)) = (
        (deps.env)("GIT_AUTHOR_NAME").map(|v| v.to_string_lossy().into_owned()),
        (deps.env)("GIT_AUTHOR_EMAIL").map(|v| v.to_string_lossy().into_owned()),
    ) {
        let identity = GitIdentity { name, email };
        return proto::check_identity(&identity).is_ok().then_some(identity);
    }
    let read = |key: &str| -> Option<String> {
        let git = ["/usr/bin/git", "/usr/local/bin/git", "/opt/homebrew/bin/git"]
            .into_iter()
            .find(|path| Path::new(path).is_file())?;
        let out = Command::new(git)
            .args(["config", "--global", "--get", key])
            .env_clear()
            .envs((deps.env)("HOME").map(|home| ("HOME", home)))
            .current_dir("/")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let value = String::from_utf8(out.stdout).ok()?.trim().to_owned();
        (out.status.success() && !value.is_empty()).then_some(value)
    };
    let identity = GitIdentity { name: read("user.name")?, email: read("user.email")? };
    proto::check_identity(&identity).is_ok().then_some(identity)
}

fn print_report(report: &GitReport, deps: &mut Deps) {
    let output = report.output.trim_end();
    if !output.is_empty() {
        say!(deps, "{output}");
    }
    for update in &report.updates {
        say!(deps, "pushed {} {} -> {}", update.name, short(&update.old), short(&update.new));
    }
    if let Some(branch) = &report.branch {
        say!(deps, "on branch {branch}");
    }
    if let Some(reason) = &report.refused {
        tell!(deps, "git: refused: {reason}");
    } else if !report.ok {
        match report.code {
            Some(code) => {
                tell!(deps, "git: Git exited with {code} in the sandbox.");
            }
            None => {
                tell!(deps, "git: Git did not finish in the sandbox.");
            }
        }
    }
}

fn short(id: &str) -> &str {
    &id[..id.len().min(10)]
}
