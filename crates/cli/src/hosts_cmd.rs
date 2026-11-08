//! `net hosts` lists the hostnames a project's sandbox may reach: those its
//! plan brings and those a person approved. `net block` takes an approval back.
//! Blocking only narrows what the sandbox reaches, so nobody is asked.

use std::path::PathBuf;

use clap::Args;
use proto::{ApproveHostArgs, HostsReport, ProjectArgs, Reply, Request, SandboxState};

use crate::ui::{esc, paint, Tone};
use crate::vm::{request, resolve, unexpected};
use crate::{exit, say, Deps};

#[derive(Args)]
pub struct HostsArgsCli {
    /// Project folder or SSH remote [default: the current folder]
    #[arg(long)]
    project: Option<PathBuf>,
}

#[derive(Args)]
pub struct BlockArgsCli {
    /// The approved hostname to block again, such as api.example.com
    host: String,
    /// Project folder or SSH remote [default: the current folder]
    #[arg(long)]
    project: Option<PathBuf>,
}

pub fn hosts(args: &HostsArgsCli, deps: &mut Deps) -> i32 {
    const COMMAND: &str = "net hosts";
    let Some(root) = resolve(args.project.as_deref(), COMMAND, deps) else {
        return exit::FAILED;
    };
    let report = match request(
        deps,
        Request::Hosts(ProjectArgs {
            project: root.to_string_lossy().into_owned(),
        }),
        COMMAND,
    ) {
        Ok(Reply::Hosts(report)) => report,
        Ok(_) => return unexpected(COMMAND, deps),
        Err(code) => return code,
    };
    for line in lines(&report) {
        say!(deps, "{}", paint_line(deps, &line));
    }
    exit::OK
}

pub fn block(args: &BlockArgsCli, deps: &mut Deps) -> i32 {
    const COMMAND: &str = "net block";
    let Some(root) = resolve(args.project.as_deref(), COMMAND, deps) else {
        return exit::FAILED;
    };
    let report = match request(
        deps,
        Request::BlockHost(ApproveHostArgs {
            project: root.to_string_lossy().into_owned(),
            host: args.host.clone(),
        }),
        COMMAND,
    ) {
        Ok(Reply::BlockHost(report)) => report,
        Ok(_) => return unexpected(COMMAND, deps),
        Err(code) => return code,
    };
    let host = esc(&report.host);
    if !report.was_approved {
        say!(deps, "{host} was not approved for this project. Nothing changed.");
        say!(deps, "See what is: {} net hosts", brand::CLI_BIN);
    } else if report.applied {
        say!(deps, "{host} is blocked again. The sandbox was replaced in {}s, with its data kept.", report.seconds);
    } else {
        say!(deps, "{host} is no longer approved. There is no sandbox yet, so the next start leaves it out.");
    }
    exit::OK
}

/// A line and how it is shown.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Line {
    Heading(String),
    Plain(String),
    Dim(String),
}

fn paint_line(deps: &Deps, line: &Line) -> String {
    match line {
        Line::Heading(text) => paint(deps.color, Tone::Strong, text),
        Line::Plain(text) => text.clone(),
        Line::Dim(text) => paint(deps.color, Tone::Dim, text),
    }
}

/// The report in words, shared with the terminal interface. Every name is
/// escaped here.
pub(crate) fn lines(report: &HostsReport) -> Vec<Line> {
    let mut out = vec![Line::Heading("Approved by you".to_owned())];
    if report.approved.is_empty() {
        out.push(Line::Dim("  None.".to_owned()));
    }
    for host in &report.approved {
        let when = host
            .approved_unix
            .map_or_else(String::new, |unix| format!(", approved {}", proto::utc_minute(unix)));
        let state = if host.in_effect {
            "reachable now"
        } else if matches!(report.sandbox, SandboxState::None) {
            "from the next start"
        } else {
            "not yet in the sandbox"
        };
        out.push(Line::Plain(format!("  {}  ({state}{when})", esc(&host.host))));
    }
    if !report.leaving.is_empty() {
        out.push(Line::Plain(String::new()));
        out.push(Line::Heading("Blocked again, still reachable until it is replaced".to_owned()));
        for host in &report.leaving {
            out.push(Line::Plain(format!("  {}", esc(host))));
        }
    }
    out.push(Line::Plain(String::new()));
    out.push(Line::Heading(if report.plan_hosts_while_running {
        "From the plan, for the build, and still reachable while it runs".to_owned()
    } else {
        "From the plan, for the build".to_owned()
    }));
    if let Some(note) = &report.plan_note {
        out.push(Line::Dim(format!("  Could not read the plan: {}", esc(note))));
    } else if report.plan_hosts.is_empty() {
        out.push(Line::Dim("  None.".to_owned()));
    }
    for host in &report.plan_hosts {
        out.push(Line::Dim(format!("  {}", esc(host))));
    }
    out.push(Line::Plain(String::new()));
    out.push(Line::Dim(
        format!("Everything else is refused. Block an approved host again with: {} net block NAME", brand::CLI_BIN),
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proto::ApprovedHost;

    fn report() -> HostsReport {
        HostsReport {
            sandbox: SandboxState::Running,
            plan_hosts: vec!["registry-1.docker.io".into()],
            plan_hosts_while_running: true,
            plan_note: None,
            approved: vec![
                ApprovedHost { host: "example.com".into(), approved_unix: Some(0), in_effect: true },
                ApprovedHost { host: "evil\u{1b}[2J.example".into(), approved_unix: None, in_effect: false },
            ],
            leaving: vec!["old.example.org".into()],
        }
    }

    #[test]
    fn approved_hosts_come_first_with_when_and_whether_they_are_reachable() {
        let text: Vec<String> = lines(&report())
            .into_iter()
            .map(|l| match l {
                Line::Heading(t) | Line::Plain(t) | Line::Dim(t) => t,
            })
            .collect();
        assert_eq!(text[0], "Approved by you");
        assert_eq!(text[1], "  example.com  (reachable now, approved 1970-01-01 00:00 UTC)");
        assert!(text[2].contains("not yet in the sandbox"));
        assert!(!text.join("\n").contains('\u{1b}'), "a name cannot carry a control code");
        assert!(text.iter().any(|t| t == "  old.example.org"));
        assert!(text.iter().any(|t| t.contains("still reachable while it runs")));
        assert!(text.last().unwrap().contains(&format!("{} net block", brand::CLI_BIN)));
    }
}
