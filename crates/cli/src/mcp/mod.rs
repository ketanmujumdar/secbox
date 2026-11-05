//! `mcp`: lets an AI agent that runs on the host work inside a sandbox.
//!
//! The project's code lives in the sandbox, so an agent on the host (Claude
//! Code, Codex, or anything else that speaks MCP) cannot see it. `mcp serve`
//! is an MCP server on stdin and stdout that the agent starts as a child
//! process. Every tool it offers acts inside the sandbox of one project,
//! through the same daemon requests the command line uses:
//!
//! - It never opens a port. A server on `localhost` could be reached by any
//!   local process and by web pages through DNS rebinding; a child process on
//!   pipes can only be reached by the agent that started it.
//! - It is bound to one project, named when it starts. A tool call cannot name
//!   another project or a path outside the sandbox's `/work`.
//! - It has no power the command line lacks. It reads no secret, changes no
//!   policy, starts and stops nothing, and approves nothing.
//!
//! `mcp setup` writes the configuration that makes an agent use it. That half
//! matters as much: MCP adds a way into the sandbox, but it does not take away
//! the agent's own shell and file tools on the host. See `docs/MCP.md`.

mod protocol;
mod run;
mod setup;
mod tools;

use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::{exit, project_dir, tell, Deps};

#[derive(Args)]
pub struct McpArgs {
    #[command(subcommand)]
    command: McpCommand,
}

#[derive(Subcommand)]
enum McpCommand {
    /// Serve MCP tools for one project's sandbox on stdin and stdout. An AI agent starts this, not a person
    Serve(ServeArgs),
    /// Set up Claude Code or Codex on this machine to work in a project's sandbox
    Setup(setup::SetupArgs),
}

#[derive(Args)]
struct ServeArgs {
    /// The project whose sandbox the tools act on. Required, so the server is
    /// never bound to whatever folder the agent happened to start in
    #[arg(long, value_name = "PATH")]
    project: PathBuf,
}

pub fn run(args: &McpArgs, deps: &mut Deps) -> i32 {
    match &args.command {
        McpCommand::Serve(args) => serve(args, deps),
        McpCommand::Setup(args) => setup::run(args, deps),
    }
}

fn serve(args: &ServeArgs, deps: &mut Deps) -> i32 {
    // Resolved once. Every tool call after this uses this exact folder, so
    // nothing the agent sends can move the server to another project.
    let project = match project_dir(Some(&args.project), deps) {
        Ok(project) => project,
        Err(reason) => {
            tell!(deps, "mcp serve: {reason}");
            return exit::FAILED;
        }
    };
    // Stdout carries the protocol and nothing else, so every human-readable
    // word goes to stderr. Agents keep it in their own logs.
    tell!(
        deps,
        "{} mcp serve: serving tools for the sandbox of {}, on stdin and stdout.",
        brand::CLI_BIN,
        crate::ui::esc_path(&project)
    );
    protocol::serve(&project, deps)
}
