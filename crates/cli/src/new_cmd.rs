//! `new`: the front door for bringing a project in, from an SSH remote or a
//! folder.
//!
//! It has no powers of its own. It sends the same requests as `up`, in the
//! same order: a fetch for a remote, then the plan, then a start that carries
//! the plan's digest and the ports the person approved. The daemon checks all
//! of it again. What differs is only how the plan is put to the person: as
//! what the project will be able to do, and one question, which defaults to
//! no and is asked only on a terminal.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clap::Args;
use proto::{
    FetchReport, PlanArgs, PlanReport, Reply, Request, SecretRoute, Source, UpArgs, UpReport,
};

use crate::ui::{esc, human_bytes, paint, Tone};
use crate::{exit, say, tell, vm, Deps};

const COMMAND: &str = "new";
const LABEL: usize = 11;
/// As many findings as `up` lists before pointing at `check`.
const MAX_FINDINGS: usize = 20;

#[derive(Args)]
pub struct NewArgsCli {
    /// An SSH remote such as git@github.com:org/repo.git, which the daemon
    /// fetches on this machine with your SSH key, or a project folder such as . or ~/code/app
    #[arg(value_name = "GIT-URL | PATH")]
    source: PathBuf,
    /// With an SSH remote: the branch, tag or full commit id to fetch [default: the remote's default branch]
    #[arg(long = "ref", value_name = "REF")]
    git_ref: Option<String>,
    /// Open only this port on 127.0.0.1. Repeat for more. Only ports the plan lists [default: every port it lists]
    #[arg(long = "port", value_name = "PORT", conflicts_with = "no_ports")]
    ports: Vec<u16>,
    /// Open no ports on 127.0.0.1
    #[arg(long)]
    no_ports: bool,
    /// Start without asking. Needed when there is no terminal to ask on. The review is printed either way
    #[arg(long)]
    yes: bool,
}

/// Where the files came from, which changes how a few steps are worded.
enum Origin {
    Remote { url: String },
    Folder,
}

pub fn run(args: &NewArgsCli, deps: &mut Deps) -> i32 {
    let remote =
        crate::names_remote(&args.source, deps).then(|| args.source.to_string_lossy().into_owned());
    let (root, origin) = match remote {
        Some(url) => {
            say!(
                deps,
                "{} Fetching {} on this machine, with your SSH key",
                dot(deps),
                esc(&url)
            );
            let Some(report) = vm::fetch_report(
                &url,
                args.git_ref.as_deref(),
                COMMAND,
                deps,
                &mut |_, _, _| {},
            ) else {
                return exit::FAILED;
            };
            fetched(&report, args.git_ref.is_some(), deps);
            (PathBuf::from(&report.project_path), Origin::Remote { url })
        }
        None => {
            if args.git_ref.is_some() {
                tell!(
                    deps,
                    "{COMMAND}: --ref is for an SSH remote. A folder starts from its last commit."
                );
                return exit::USAGE;
            }
            let Some(root) = vm::resolve(Some(&args.source), COMMAND, deps) else {
                return exit::FAILED;
            };
            (root, Origin::Folder)
        }
    };

    // The same plan `up` asks for: the last commit, nothing uncommitted.
    let source = Source::default();
    let plan = match vm::call_daemon(
        deps,
        Request::Plan(PlanArgs {
            project: root.to_string_lossy().into_owned(),
            source: source.clone(),
        }),
        COMMAND,
        &mut |_, _, _| {},
    ) {
        Ok(Reply::Plan(plan)) => *plan,
        Ok(_) => return vm::unexpected(COMMAND, deps),
        Err(code) => return code,
    };

    if let Origin::Folder = origin {
        let ok = tick(deps);
        say!(
            deps,
            "{ok} Took {} from its last commit (uncommitted changes stay out)",
            esc(&plan.project_path)
        );
    }
    read_line(&plan, deps);

    let ports = if args.no_ports {
        Vec::new()
    } else if args.ports.is_empty() {
        plan.ports.iter().map(|p| p.host_port).collect()
    } else {
        match vm::listed_ports(&plan, &args.ports, COMMAND, deps) {
            Ok(ports) => ports,
            Err(code) => return code,
        }
    };

    say!(deps);
    review(&plan, &ports, args.no_ports, deps);
    say!(
        deps,
        "plan {} · {} shows all of it{}",
        short_digest(&plan.digest),
        full_plan_hint(&origin, &root),
        match origin {
            Origin::Remote { .. } => ", after fetching again",
            Origin::Folder => "",
        }
    );

    if !plan.blockers.is_empty() {
        say!(deps);
        say!(
            deps,
            "{}",
            paint(deps.color, Tone::Bad, "This cannot be started yet")
        );
        for blocker in &plan.blockers {
            say!(deps, "  {}", esc(blocker));
        }
        say!(deps);
        say!(deps, "Nothing was started.");
        return exit::FAILED;
    }

    say!(deps);
    if args.yes {
        say!(deps, "Starting with this plan, because of --yes.");
    } else if !deps.terminal {
        tell!(
            deps,
            "{COMMAND}: nothing was started. There is no terminal to ask on, so starting needs --yes, given after reading the review above."
        );
        return exit::FAILED;
    } else if !ask(deps) {
        say!(deps, "Nothing was started.");
        return exit::FAILED;
    }

    let remote = matches!(origin, Origin::Remote { .. });
    let has_secrets = !plan.secrets.is_empty();
    let mut current: Option<(String, Instant)> = None;
    let answer = vm::call_daemon(
        deps,
        Request::Up(UpArgs {
            project: root.to_string_lossy().into_owned(),
            source,
            plan_digest: plan.digest.clone(),
            approve_ports: ports,
        }),
        COMMAND,
        &mut |deps, step, _detail| {
            if let Some((done, since)) = current.take() {
                finished(deps, &done, since.elapsed(), remote, has_secrets);
            }
            if let Some(line) = starting(step) {
                say!(deps, "{} {line}", dot(deps));
            }
            current = Some((step.to_owned(), Instant::now()));
        },
    );
    let report = match answer {
        Ok(Reply::Up(report)) => report,
        Ok(_) => return vm::unexpected(COMMAND, deps),
        Err(code) => {
            if let Some((step, _)) = current {
                let bad = paint(deps.color, Tone::Bad, "✗");
                say!(deps, "{bad} {} did not finish", esc(&step));
            }
            return code;
        }
    };
    if let Some((done, since)) = current.take() {
        finished(deps, &done, since.elapsed(), remote, has_secrets);
    }
    started(&plan, &report, deps);
    next(&origin, &root, deps);
    exit::OK
}

/// What the fetch got, in one line, and what was not done.
fn fetched(report: &FetchReport, asked_for_ref: bool, deps: &mut Deps) {
    let ok = tick(deps);
    let commit = report
        .commit
        .as_deref()
        .map(|c| esc(c.get(..7).unwrap_or(c)))
        .unwrap_or_else(|| "an unknown commit".to_owned());
    let what = if report.git_ref == "HEAD" {
        "the default branch".to_owned()
    } else {
        esc(&report.git_ref)
    };
    if report.fetched {
        say!(
            deps,
            "{ok} Fetched {what} @ {commit} with your SSH key (the key stays here)"
        );
    } else {
        say!(
            deps,
            "{ok} Already fetched @ {commit}: a sandbox holds it, so it was not fetched again"
        );
        if asked_for_ref {
            say!(deps, "  --ref was not used for the same reason. Switch branches inside the sandbox with git.");
        }
        say!(
            deps,
            "  Bring in newer commits with: {} git pull --project {}",
            brand::CLI_BIN,
            esc(&report.url)
        );
    }
}

/// What the plan was read from. Nothing from the project has run.
fn read_line(plan: &PlanReport, deps: &mut Deps) {
    let ok = tick(deps);
    let count = match plan.services.len() {
        1 => "1 service".to_owned(),
        n => format!("{n} services"),
    };
    let from = if plan.generated_compose.is_some() {
        format!(
            "Dockerfile: {count}, through a Compose file {} wrote for it",
            brand::PRODUCT_NAME
        )
    } else if plan.compose_files.is_empty() {
        format!("the project ({}): {count}", esc(&plan.kind))
    } else {
        let files: Vec<String> = plan.compose_files.iter().map(|f| esc(f)).collect();
        format!("{}: {count}", files.join(" + "))
    };
    say!(deps, "{ok} Read {from} (nothing has run yet)");
}

/// The plan, put as what the project will be able to do.
fn review(plan: &PlanReport, ports: &[u16], no_ports: bool, deps: &mut Deps) {
    let name = Path::new(&plan.project_path)
        .file_name()
        .map(|n| esc(&n.to_string_lossy()))
        .unwrap_or_else(|| esc(&plan.project_path));
    say!(
        deps,
        "{}",
        paint(
            deps.color,
            Tone::Strong,
            &format!("{name} will be able to…")
        )
    );

    // reach
    let network = &plan.network;
    let build = wrap(&network.build_hosts);
    let run = wrap(&network.run_hosts);
    // The time goes after the first line of each, so only those line up.
    let width = build
        .first()
        .into_iter()
        .chain(run.first())
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
    let when_building = if network.merged_for_whole_life {
        "while building, and after"
    } else {
        "while building"
    };
    let mut reach: Vec<String> = Vec::new();
    for (lines, when) in [(&build, when_building), (&run, "while running")] {
        for (i, line) in lines.iter().enumerate() {
            if i == 0 {
                reach.push(format!("{line:<width$}   {when}"));
            } else {
                reach.push(line.clone());
            }
        }
    }
    if reach.is_empty() {
        reach.push("nothing: every outgoing connection is refused".to_owned());
    }
    group(deps, "reach", &reach);

    // open
    let mut open: Vec<String> = Vec::new();
    let mut closed: Vec<String> = Vec::new();
    for port in &plan.ports {
        let inside = if port.container_port == port.host_port {
            String::new()
        } else {
            format!(" (port {} inside)", port.container_port)
        };
        let text = format!(
            "127.0.0.1:{} → {}{inside}",
            port.host_port,
            esc(&port.service)
        );
        if ports.contains(&port.host_port) {
            open.push(text);
        } else {
            closed.push(text);
        }
    }
    let mut open_lines = Vec::new();
    if !open.is_empty() {
        open_lines.push(open.join("    "));
    }
    if !closed.is_empty() {
        let why = if no_ports {
            "--no-ports"
        } else {
            "not chosen with --port"
        };
        open_lines.push(format!("not opened ({why}): {}", closed.join("    ")));
    }
    if open_lines.is_empty() {
        open_lines.push("no ports: the project publishes none".to_owned());
    }
    group(deps, "open", &open_lines);

    // get
    let mut get: Vec<String> = plan
        .secrets
        .iter()
        .map(|secret| {
            let route = match secret.route {
                SecretRoute::Proxy => "held by the proxy",
                SecretRoute::Injected => "in the environment",
                SecretRoute::InjectedFile => "a file at /run/secrets",
                SecretRoute::BuildSecret => "a build secret",
            };
            let to = if secret.services.is_empty() {
                "no service".to_owned()
            } else {
                secret
                    .services
                    .iter()
                    .map(|s| esc(s))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            format!("{} → {to} ({route})", esc(&secret.name))
        })
        .collect();
    if get.is_empty() {
        get.push("no secrets".to_owned());
    }
    group(deps, "get", &get);

    // use
    let r = &plan.resources;
    let building = if r.build_memory_bytes != r.memory_bytes {
        format!("  ({} while building)", human_bytes(r.build_memory_bytes))
    } else {
        String::new()
    };
    let clamped = if r.clamped {
        ", lowered to your maximum"
    } else {
        ""
    };
    group(
        deps,
        "use",
        &[format!(
            "{} memory · {} CPUs · {} disk{building}{clamped}",
            human_bytes(r.memory_bytes),
            f64::from(r.millicpus) / 1000.0,
            human_bytes(r.disk_bytes)
        )],
    );

    warnings(plan, ports, deps);
}

/// Findings, warnings and where secrets could go, each on a `!` line.
fn warnings(plan: &PlanReport, ports: &[u16], deps: &mut Deps) {
    let mut lines: Vec<String> = Vec::new();
    let high: Vec<&proto::Finding> = plan
        .findings
        .iter()
        .filter(|f| f.severity == "Critical" || f.severity == "High")
        .collect();
    for finding in high.iter().take(MAX_FINDINGS) {
        let service = finding
            .service
            .as_deref()
            .map(|s| format!("{}: ", esc(s)))
            .unwrap_or_default();
        lines.push(format!(
            "{service}{} ({})",
            esc(&finding.message),
            esc(&finding.severity)
        ));
    }
    if high.len() > MAX_FINDINGS {
        lines.push(format!(
            "... and {} more. {} check shows all of them.",
            high.len() - MAX_FINDINGS,
            brand::CLI_BIN
        ));
    }
    if plan.network.merged_for_whole_life && !plan.network.build_hosts.is_empty() {
        lines.push(
            "build hosts stay reachable after the build, because the sandbox's rules cannot be changed once it exists"
                .to_owned(),
        );
    }
    if plan.network.tls == proto::TlsPlan::Intercept {
        lines.push("the gateway terminates TLS for this sandbox".to_owned());
    }
    let opened: Vec<&proto::PortPlan> = plan
        .ports
        .iter()
        .filter(|p| ports.contains(&p.host_port))
        .collect();
    for port in opened.iter().filter(|p| p.dynamic) {
        lines.push(format!(
            "port {} comes from a variable, so it can be different when it starts",
            port.host_port
        ));
    }
    let everywhere: Vec<String> = opened
        .iter()
        .filter(|p| p.was_all_interfaces)
        .map(|p| p.host_port.to_string())
        .collect();
    if !everywhere.is_empty() {
        lines.push(format!(
            "the project asks for {} on every interface; only 127.0.0.1 is used",
            everywhere.join(", ")
        ));
    }
    lines.extend(plan.snapshot.warnings.iter().map(|w| esc(w)));
    for line in &lines {
        say!(deps, "{} {line}", bang(deps));
    }

    // The same words as every other client, so "where can this key go" has
    // one answer.
    let exposure = proto::exposure_lines(
        &proto::held_from_plan(&plan.secrets),
        Some(&proto::reachable_while_running(&plan.network)),
        true,
    );
    let mut list_next = false;
    for line in exposure {
        if list_next {
            let hosts: Vec<String> = line.split(", ").map(str::to_owned).collect();
            for part in wrap(&hosts) {
                say!(deps, "  {part}");
            }
        } else {
            say!(deps, "{} {}", bang(deps), esc(&line));
        }
        list_next = line.ends_with(':');
    }
}

/// Asks once. Only `y` or `yes` starts it.
fn ask(deps: &mut Deps) -> bool {
    let _ = write!(deps.out, "Start with this plan? [y/N] ");
    let _ = deps.out.flush();
    let mut answer = String::new();
    if deps.input.read_line(&mut answer).is_err() {
        say!(deps);
        return false;
    }
    let answer = answer.trim();
    answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
}

/// A line to print when a slow step begins.
fn starting(step: &str) -> Option<&'static str> {
    match step {
        "build" => {
            Some("Building images and starting services. The first start can take several minutes")
        }
        _ => None,
    }
}

/// A step is over once the next one begins, or the start finishes.
fn finished(deps: &mut Deps, step: &str, took: Duration, remote: bool, has_secrets: bool) {
    let what = match step {
        "planning" => "Plan checked again: it is the one you approved".to_owned(),
        "sandbox" => "Virtual machine started".to_owned(),
        "snapshot" if remote => {
            "Cloned into the VM through the relay, with no key inside".to_owned()
        }
        "snapshot" => "Last commit copied into the VM".to_owned(),
        "docker" => "Docker running inside the VM".to_owned(),
        "secrets" if has_secrets => "Secrets delivered".to_owned(),
        "secrets" => "No secrets to deliver".to_owned(),
        "build" => "Images built and services started".to_owned(),
        other => esc(other),
    };
    let ok = tick(deps);
    let took = paint(deps.color, Tone::Dim, &duration(took));
    say!(deps, "{ok} {what}  {took}");
}

/// What the daemon says is now running. The sandbox reported the services
/// itself, so they are information rather than proof.
fn started(plan: &PlanReport, report: &UpReport, deps: &mut Deps) {
    for port in &report.ports {
        let service = plan
            .ports
            .iter()
            .find(|p| p.host_port == *port)
            .map(|p| format!(" → {}", esc(&p.service)))
            .unwrap_or_default();
        let ok = tick(deps);
        say!(deps, "{ok} Opened 127.0.0.1:{port}{service}");
    }
    if report.ports.is_empty() {
        say!(deps, "{} No ports opened", dot(deps));
    }
    for delivered in &report.secrets {
        say!(
            deps,
            "  {} → {}",
            delivered
                .names
                .iter()
                .map(|n| esc(n))
                .collect::<Vec<_>>()
                .join(", "),
            esc(&delivered.service)
        );
    }
    for service in &report.services {
        let health = service
            .health
            .as_deref()
            .map(|h| format!(" ({})", esc(h)))
            .unwrap_or_default();
        say!(
            deps,
            "  {} {}{health}",
            esc(&service.name),
            esc(&service.state)
        );
    }
    say!(deps);
    say!(deps, "Started in {}s.", report.seconds);
    say!(
        deps,
        "{}",
        paint(
            deps.color,
            Tone::Warn,
            "What runs inside the sandbox is untrusted. Its own reports can be wrong."
        )
    );
    if !report.secrets.is_empty() {
        say!(
            deps,
            "{}",
            paint(
                deps.color,
                Tone::Warn,
                "A service can read every secret it was given, and so can anything sharing its container."
            )
        );
    }
}

/// Real commands to go on with, naming the project the way they take it.
fn next(origin: &Origin, root: &Path, deps: &mut Deps) {
    let project = project_arg(origin, root);
    let bin = brand::CLI_BIN;
    let lines = [
        (bin.to_owned(), "opens the full-screen view"),
        (
            format!("{bin} logs --project {project}"),
            "shows what the services print",
        ),
        (
            format!("{bin} down {project}"),
            "stops it, keeping its data",
        ),
    ];
    let width = lines
        .iter()
        .map(|(c, _)| c.chars().count())
        .max()
        .unwrap_or(0);
    say!(deps);
    for (i, (command, what)) in lines.iter().enumerate() {
        let head = if i == 0 { "Next:" } else { "" };
        say!(deps, "{head:<6}{command:<width$}   {what}");
    }
}

/// Hostnames joined with commas, a few to a line.
fn wrap(hosts: &[String]) -> Vec<String> {
    const WIDTH: usize = 56;
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for host in hosts.iter().map(|h| esc(h)) {
        if !line.is_empty() && line.chars().count() + host.chars().count() + 2 > WIDTH {
            line.push(',');
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push_str(", ");
        }
        line.push_str(&host);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn full_plan_hint(origin: &Origin, root: &Path) -> String {
    format!(
        "{} up {} --plan-only",
        brand::CLI_BIN,
        project_arg(origin, root)
    )
}

fn project_arg(origin: &Origin, root: &Path) -> String {
    match origin {
        Origin::Remote { url } => esc(url),
        Origin::Folder => crate::ui::esc_path(root),
    }
}

fn group(deps: &mut Deps, label: &str, lines: &[String]) {
    for (i, line) in lines.iter().enumerate() {
        let head = if i == 0 { label } else { "" };
        say!(
            deps,
            "  {}{line}",
            paint(deps.color, Tone::Strong, &format!("{head:<LABEL$}"))
        );
    }
}

fn short_digest(digest: &str) -> String {
    let bare = digest.split_once(':').map_or(digest, |(_, hex)| hex);
    esc(bare.get(..8).unwrap_or(bare))
}

fn duration(took: Duration) -> String {
    let seconds = took.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {}s", seconds / 60, seconds % 60)
    }
}

fn tick(deps: &Deps) -> String {
    paint(deps.color, Tone::Good, "✓")
}

fn dot(deps: &Deps) -> String {
    paint(deps.color, Tone::Dim, "·")
}

fn bang(deps: &Deps) -> String {
    paint(deps.color, Tone::Warn, "!")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_like_the_clock() {
        assert_eq!(duration(Duration::from_secs(4)), "4s");
        assert_eq!(duration(Duration::from_secs(72)), "1m 12s");
    }

    #[test]
    fn a_digest_is_shortened_without_its_algorithm() {
        assert_eq!(short_digest("sha256:9e1d4c07aa"), "9e1d4c07");
        assert_eq!(short_digest("9e1d4c07aa"), "9e1d4c07");
        assert_eq!(short_digest("abc"), "abc");
    }
}
