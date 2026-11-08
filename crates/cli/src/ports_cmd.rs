//! `ports`: show a project's ports, open or close one, or move it to another
//! number on this Mac. A choice changes no sandbox. The next start applies it,
//! and that start asks the person to confirm, naming the ports it opens.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use proto::{PortChoice, PortChoicesReport, ProjectArgs, Reply, Request, SetPortArgs};

use crate::ui::{esc, paint, Tone};
use crate::vm::{call_daemon, resolve, unexpected};
use crate::{exit, say, tell, Deps};

const COMMAND: &str = "ports";

#[derive(Args)]
pub struct PortsArgs {
    #[command(subcommand)]
    command: Option<PortsCommand>,
}

#[derive(Subcommand)]
enum PortsCommand {
    /// Show each port the project publishes, and whether it is open [the default]
    Show(ShowArgs),
    /// Open a port at the next start, on its own number or another one
    Open(OpenArgs),
    /// Close a port at the next start
    Close(CloseArgs),
}

#[derive(Args)]
struct ShowArgs {
    /// Project folder [default: the current folder]
    path: Option<PathBuf>,
}

#[derive(Args)]
struct OpenArgs {
    /// The port, as the project publishes it (3000), or as service:port (web:80)
    port: String,
    /// The number to use on this Mac, such as 3001, when its own is taken
    #[arg(long = "on", value_name = "PORT")]
    on: Option<u16>,
    /// Project folder [default: the current folder]
    path: Option<PathBuf>,
}

#[derive(Args)]
struct CloseArgs {
    /// The port, as the project publishes it (3000), or as service:port (web:80)
    port: String,
    /// Project folder [default: the current folder]
    path: Option<PathBuf>,
}

pub fn run(args: &PortsArgs, deps: &mut Deps) -> i32 {
    match &args.command {
        None => show(None, deps),
        Some(PortsCommand::Show(args)) => show(args.path.as_deref(), deps),
        Some(PortsCommand::Open(args)) => {
            change(args.path.as_deref(), &args.port, true, args.on, deps)
        }
        Some(PortsCommand::Close(args)) => {
            change(args.path.as_deref(), &args.port, false, None, deps)
        }
    }
}

fn quiet(_: &mut Deps, _: &str, _: Option<&str>) {}

fn choices(root: &std::path::Path, deps: &mut Deps) -> Result<PortChoicesReport, i32> {
    let request = Request::PortChoices(ProjectArgs {
        project: root.to_string_lossy().into_owned(),
    });
    match call_daemon(deps, request, COMMAND, &mut quiet)? {
        Reply::PortChoices(report) => Ok(report),
        _ => Err(unexpected(COMMAND, deps)),
    }
}

fn show(path: Option<&std::path::Path>, deps: &mut Deps) -> i32 {
    let Some(root) = resolve(path, COMMAND, deps) else {
        return exit::FAILED;
    };
    match choices(&root, deps) {
        Ok(report) => {
            print(&report, deps);
            exit::OK
        }
        Err(code) => code,
    }
}

/// Finds the port a person named: its own number, or `service:port`.
fn find<'a>(ports: &'a [PortChoice], named: &str) -> Option<&'a PortChoice> {
    if let Some((service, port)) = named.split_once(':') {
        let port: u16 = port.parse().ok()?;
        return ports
            .iter()
            .find(|p| p.service == service && p.container_port == port);
    }
    let port: u16 = named.parse().ok()?;
    ports.iter().find(|p| p.port == port)
}

fn change(
    path: Option<&std::path::Path>,
    named: &str,
    open: bool,
    on: Option<u16>,
    deps: &mut Deps,
) -> i32 {
    let Some(root) = resolve(path, COMMAND, deps) else {
        return exit::FAILED;
    };
    let report = match choices(&root, deps) {
        Ok(report) => report,
        Err(code) => return code,
    };
    if let Some(problem) = &report.problem {
        tell!(deps, "{COMMAND}: {}", esc(problem));
        return exit::FAILED;
    }
    let Some(port) = find(&report.ports, named) else {
        let publishes: Vec<String> = report
            .ports
            .iter()
            .map(|p| format!("{} ({}:{})", p.port, esc(&p.service), p.container_port))
            .collect();
        tell!(
            deps,
            "{COMMAND}: {} is not a port this project publishes. It publishes {}.",
            esc(named),
            if publishes.is_empty() {
                "nothing".to_owned()
            } else {
                publishes.join(", ")
            }
        );
        return exit::USAGE;
    };
    let request = Request::SetPort(SetPortArgs {
        project: root.to_string_lossy().into_owned(),
        port: port.port,
        open,
        mac_port: on,
    });
    let report = match call_daemon(deps, request, COMMAND, &mut quiet) {
        Ok(Reply::SetPort(report)) => report,
        Ok(_) => return unexpected(COMMAND, deps),
        Err(code) => return code,
    };
    say!(deps, "Saved. Nothing changes until it next starts, and that start asks you to confirm the ports it opens.");
    say!(deps);
    print(&report, deps);
    exit::OK
}

fn print(report: &PortChoicesReport, deps: &mut Deps) {
    if let Some(problem) = &report.problem {
        say!(deps, "Cannot read the ports: {}", esc(problem));
        return;
    }
    if report.ports.is_empty() {
        say!(deps, "This project publishes no ports.");
        return;
    }
    say!(deps, "{}", paint(deps.color, Tone::Strong, "Ports"));
    for port in &report.ports {
        let name = format!(
            "{} ({}:{})",
            port.port,
            esc(&port.service),
            port.container_port
        );
        let next = if port.open {
            paint(
                deps.color,
                Tone::Good,
                &format!("open on 127.0.0.1:{}", port.mac_port),
            )
        } else {
            paint(deps.color, Tone::Dim, "closed")
        };
        let now = match port.open_now {
            Some(now) if !(port.open && now == port.mac_port) => {
                format!(", now open on 127.0.0.1:{now}")
            }
            None if port.open => ", now closed".to_owned(),
            _ => String::new(),
        };
        say!(deps, "  {name:<24} {next}{now}");
        if let Some(why) = &port.cannot_open {
            say!(
                deps,
                "  {:<24} {}",
                "",
                paint(deps.color, Tone::Warn, &esc(why))
            );
        }
    }
    say!(deps);
    if report.restart_needed {
        say!(
            deps,
            "It is running with other ports. Restart it to apply them: {0} down, then {0} up.",
            brand::CLI_BIN
        );
    }
    say!(
        deps,
        "Change one with: {0} ports open 3000 --on 3001, or {0} ports close 3000.",
        brand::CLI_BIN
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(service: &str, port: u16, container_port: u16) -> PortChoice {
        PortChoice {
            service: service.to_owned(),
            container_port,
            port,
            mac_port: port,
            open: false,
            open_now: None,
            cannot_open: None,
        }
    }

    #[test]
    fn a_port_is_found_by_its_number_or_by_service_and_port() {
        let ports = [choice("web", 3000, 80), choice("api", 8080, 8080)];
        assert_eq!(find(&ports, "3000").unwrap().service, "web");
        assert_eq!(find(&ports, "web:80").unwrap().port, 3000);
        assert!(find(&ports, "web:3000").is_none());
        assert!(find(&ports, "9999").is_none());
        assert!(find(&ports, "nonsense").is_none());
    }
}
