//! `up`, `down` and `status`: the commands that run a project in a sandbox.
//!
//! None of the decisions are made here. The daemon builds the plan, and this
//! shows it, asks the person, and sends back the answer together with the
//! plan's hash so that the daemon can check it is approving the same thing.

use std::path::PathBuf;

use clap::Args;
use proto::{
    ApproveHostArgs, BlockedRecording, DownArgs, PlanArgs, PlanReport, ProjectArgs, Reply, Request, SandboxState, Source, UpArgs,
};

use crate::ui::{cell, esc, human_bytes, paint, Tone};
use crate::{exit, project_dir, say, tell, Deps};

const LABEL: usize = 11;

#[derive(Args)]
pub struct UpArgsCli {
    /// Project folder, or an SSH remote such as git@github.com:org/repo.git,
    /// which the daemon fetches on this machine with your SSH key [default: the current folder]
    path: Option<PathBuf>,
    /// With an SSH remote: the branch, tag or full commit id to fetch [default: the remote's default branch]
    #[arg(long = "ref", value_name = "REF")]
    git_ref: Option<String>,
    /// Start from this commit, named by its full id [default: HEAD]
    #[arg(long, value_name = "ID")]
    commit: Option<String>,
    /// Include changes that are not committed. They reach the sandbox unreviewed
    #[arg(long)]
    include_uncommitted: bool,
    /// Forward this host port to 127.0.0.1. Repeat for more. Only ports the plan lists
    #[arg(long = "port", value_name = "PORT")]
    ports: Vec<u16>,
    /// Forward every port in the plan, without asking
    #[arg(long)]
    all_ports: bool,
    /// Show the plan and stop
    #[arg(long)]
    plan_only: bool,
}

#[derive(Args)]
pub struct DownArgsCli {
    /// Project folder [default: the current folder]
    path: Option<PathBuf>,
    /// Remove the sandbox and everything in it, rather than stopping it
    #[arg(long)]
    destroy: bool,
}

#[derive(Args)]
pub struct AllowArgsCli {
    /// The exact hostname, such as api.example.com. No patterns
    host: String,
    /// Project folder or SSH remote [default: the current folder]
    #[arg(long)]
    project: Option<PathBuf>,
}

#[derive(Args)]
pub struct RemoveArgsCli {
    /// Project folder, even one that no longer exists, or an SSH remote [default: the current folder]
    path: Option<PathBuf>,
}

#[derive(Args)]
pub struct StatusArgsCli {
    /// Project folder [default: the current folder]
    path: Option<PathBuf>,
}

pub fn up(args: &UpArgsCli, deps: &mut Deps) -> i32 {
    let remote = args
        .path
        .as_deref()
        .filter(|path| crate::names_remote(path, deps))
        .map(|path| path.to_string_lossy().into_owned());
    let root = match &remote {
        Some(url) => {
            if args.include_uncommitted {
                tell!(deps, "up: a remote has no uncommitted changes, so --include-uncommitted does not apply.");
                return exit::USAGE;
            }
            if args.commit.is_some() {
                tell!(deps, "up: for a remote, name the branch, tag or commit with --ref.");
                return exit::USAGE;
            }
            match fetch(url, args.git_ref.as_deref(), deps) {
                Some(root) => root,
                None => return exit::FAILED,
            }
        }
        None => {
            if args.git_ref.is_some() {
                tell!(deps, "up: --ref is for an SSH remote. For a folder, use --commit with a full commit id.");
                return exit::USAGE;
            }
            let Some(root) = resolve(args.path.as_deref(), "up", deps) else {
                return exit::FAILED;
            };
            root
        }
    };
    let source = match (&args.commit, args.include_uncommitted) {
        (Some(_), true) => {
            tell!(
                deps,
                "up: choose one of --commit and --include-uncommitted, not both."
            );
            return exit::USAGE;
        }
        (Some(commit), false) => Source::Commit {
            revision: commit.clone(),
        },
        (None, true) => Source::WorkingTree,
        (None, false) => Source::default(),
    };

    let plan = match request(
        deps,
        Request::Plan(PlanArgs {
            project: root.to_string_lossy().into_owned(),
            source: source.clone(),
        }),
        "up",
    ) {
        Ok(Reply::Plan(plan)) => *plan,
        Ok(_) => return unexpected("up", deps),
        Err(code) => return code,
    };

    show_plan(&plan, deps);

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
    if args.plan_only {
        say!(deps);
        say!(deps, "Nothing was started, because of --plan-only.");
        return exit::OK;
    }

    let approved = match choose_ports(&plan, args, deps) {
        Ok(ports) => ports,
        Err(code) => return code,
    };

    say!(deps);
    let report = match request(
        deps,
        Request::Up(UpArgs {
            project: root.to_string_lossy().into_owned(),
            source,
            plan_digest: plan.digest.clone(),
            approve_ports: approved,
        }),
        "up",
    ) {
        Ok(Reply::Up(report)) => report,
        Ok(_) => return unexpected("up", deps),
        Err(code) => return code,
    };

    say!(deps);
    field(deps, "Started", &format!("in {}s", report.seconds));
    for delivered in &report.secrets {
        field(
            deps,
            "Secrets",
            &format!(
                "{} -> {}",
                delivered
                    .names
                    .iter()
                    .map(|n| esc(n))
                    .collect::<Vec<_>>()
                    .join(", "),
                esc(&delivered.service)
            ),
        );
    }
    show_services(&report.services, deps);
    for port in &report.ports {
        field(deps, "Ready", &format!("http://127.0.0.1:{port}"));
    }
    say!(deps);
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
    exit::OK
}

pub fn down(args: &DownArgsCli, deps: &mut Deps) -> i32 {
    let Some(root) = resolve(args.path.as_deref(), "down", deps) else {
        return exit::FAILED;
    };
    let report = match request(
        deps,
        Request::Down(DownArgs {
            project: root.to_string_lossy().into_owned(),
            destroy: args.destroy,
        }),
        "down",
    ) {
        Ok(Reply::Down(report)) => report,
        Ok(_) => return unexpected("down", deps),
        Err(code) => return code,
    };
    match (report.destroyed, report.sandbox) {
        (true, _) => {
            say!(deps, "Destroyed the sandbox and everything in it.");
            say!(
                deps,
                "Rotate every secret it was given: a build cache and a data volume can both carry a compromise."
            );
        }
        (false, SandboxState::None) => {
            say!(deps, "There was no sandbox for this project.");
        }
        (false, _) => {
            say!(deps, "Stopped. Its data is kept for the next start.");
            say!(
                deps,
                "Stopping is abrupt for what runs inside, so a database may recover on the next start."
            );
        }
    }
    exit::OK
}

pub fn allow(args: &AllowArgsCli, deps: &mut Deps) -> i32 {
    let Some(root) = resolve(args.project.as_deref(), "net allow", deps) else {
        return exit::FAILED;
    };
    let report = match request(
        deps,
        Request::ApproveHost(ApproveHostArgs {
            project: root.to_string_lossy().into_owned(),
            host: args.host.clone(),
        }),
        "net allow",
    ) {
        Ok(Reply::ApproveHost(report)) => report,
        Ok(_) => return unexpected("net allow", deps),
        Err(code) => return code,
    };
    let host = esc(&report.host);
    if report.already_allowed {
        say!(deps, "{host} was already allowed. Nothing changed.");
    } else if report.applied {
        say!(deps, "{host} is allowed now. The sandbox was replaced in {}s, with its data kept.", report.seconds);
        say!(deps, "Everything in the sandbox can reach it, including any service holding a secret.");
    } else {
        say!(deps, "{host} is approved, and applies when this project starts.");
    }
    exit::OK
}

pub fn remove(args: &RemoveArgsCli, deps: &mut Deps) -> i32 {
    let root = match (project_dir(args.path.as_deref(), deps), args.path.as_deref()) {
        (Ok(root), _) => root,
        // A folder that has gone can still be removed: the daemon only looks it up.
        (Err(_), Some(path)) if !path.exists() => deps.cwd.join(path),
        (Err(reason), _) => {
            tell!(deps, "remove: {reason}");
            return exit::FAILED;
        }
    };
    let report = match request(
        deps,
        Request::Remove(ProjectArgs {
            project: root.to_string_lossy().into_owned(),
        }),
        "remove",
    ) {
        Ok(Reply::Remove(report)) => report,
        Ok(_) => return unexpected("remove", deps),
        Err(code) => return code,
    };
    say!(deps, "Removed the project from this machine.");
    if report.removed_copy {
        say!(deps, "The imported copy is gone. The remote itself was not touched.");
    } else {
        say!(deps, "Its folder was not touched.");
    }
    if report.sandbox_destroyed {
        say!(deps, "Its sandbox was destroyed. Rotate every secret it was given.");
    }
    say!(deps, "Any secrets you stored for it stay in your keychain.");
    exit::OK
}

pub fn status(args: &StatusArgsCli, deps: &mut Deps) -> i32 {
    let Some(root) = resolve(args.path.as_deref(), "status", deps) else {
        return exit::FAILED;
    };
    let report = match request(
        deps,
        Request::Status(ProjectArgs {
            project: root.to_string_lossy().into_owned(),
        }),
        "status",
    ) {
        Ok(Reply::Status(report)) => report,
        Ok(_) => return unexpected("status", deps),
        Err(code) => return code,
    };
    field(deps, "Project", &esc(&report.project_path));
    let (tone, word) = match report.sandbox {
        SandboxState::Running => (Tone::Good, "running"),
        SandboxState::Stopped => (Tone::Warn, "stopped"),
        SandboxState::None => (Tone::Dim, "none"),
        SandboxState::Unknown => (Tone::Bad, "not understood"),
    };
    field(deps, "Sandbox", &paint(deps.color, tone, word));
    if let Some(resources) = &report.resources {
        field(deps, "Resources", &describe_resources(resources));
    }
    if !report.ports.is_empty() {
        let ports: Vec<String> = report
            .ports
            .iter()
            .map(|p| format!("127.0.0.1:{p}"))
            .collect();
        field(deps, "Ports", &ports.join(", "));
    }
    show_services(&report.services, deps);
    say!(deps);
    show_blocked(report.blocked.as_ref(), deps);
    if let Some(exposure) = &report.exposure {
        show_exposure(&exposure.secrets, exposure.reachable_hosts.as_deref(), deps);
    }
    exit::OK
}

/// Which secret goes to which service, and every host it could be sent to.
/// Printed whenever a secret is assigned, because that list is the real
/// answer to whether a key can leave.
fn show_exposure(secrets: &[proto::HeldSecret], hosts: Option<&[String]>, deps: &mut Deps) {
    let lines = proto::exposure_lines(secrets, hosts, false);
    if lines.is_empty() {
        return;
    }
    say!(deps);
    say!(deps, "{}", paint(deps.color, Tone::Warn, "Where secrets could go"));
    for line in lines {
        say!(deps, "  {}", esc(&line));
    }
}

/// Saying "0 blocked" would read as "nothing was blocked", which is a different
/// and untrue claim, so a count of none is never put that way.
fn show_blocked(summary: Option<&proto::BlockedSummary>, deps: &mut Deps) {
    let Some(summary) = summary else {
        field(
            deps,
            "Blocked",
            &format!(
                "unknown: the running daemon is older than this command. Restart {} to see this.",
                brand::DAEMON_BIN
            ),
        );
        return;
    };
    let cut = if summary.cut_off { ", older ones cut off" } else { "" };
    let line = match (&summary.problem, summary.recording) {
        (Some(problem), _) => paint(
            deps.color,
            Tone::Bad,
            &format!("unknown: the log could not be read: {}", esc(problem)),
        ),
        (None, BlockedRecording::NotRecording) => {
            paint(deps.color, Tone::Bad, &crate::blocked::not_recording_note())
        }
        (None, BlockedRecording::NoSandbox) => "nothing is being recorded: no sandbox".to_owned(),
        (None, BlockedRecording::Recording) if summary.total == 0 => {
            format!("no hostnames recorded so far{cut}")
        }
        (None, BlockedRecording::Recording) => format!(
            "{} refused lookups of {} hostnames{cut}",
            summary.total, summary.domains
        ),
    };
    field(deps, "Blocked", &line);
    if let Some((weight, text)) = crate::blocked::trust_note(&summary.trust) {
        if weight == crate::blocked::Weight::Alert {
            say!(deps, "{}", paint(deps.color, Tone::Bad, &text));
        }
    }
    say!(deps, "{}", paint(deps.color, Tone::Warn, crate::blocked::HOSTNAMES_ONLY));
}

fn show_plan(plan: &PlanReport, deps: &mut Deps) {
    field(deps, "Project", &esc(&plan.project_path));
    field(
        deps,
        "Kind",
        &format!(
            "{}, {}",
            esc(&plan.kind),
            count(plan.services.len(), "service", "services")
        ),
    );
    let snapshot = match (&plan.snapshot.revision, &plan.snapshot.counts) {
        (Some(revision), _) => format!("commit {}", short(revision)),
        (None, _) => "the working tree, including uncommitted changes".to_owned(),
    };
    field(deps, "Snapshot", &esc(&snapshot));
    field(deps, "Resources", &describe_resources(&plan.resources));

    let tls = match plan.network.tls {
        proto::TlsPlan::ServerNameOnly => "names only, no decryption",
        proto::TlsPlan::Intercept => "the gateway terminates TLS",
    };
    field(
        deps,
        "Network",
        &format!(
            "deny by default; {} for the build, {} while it runs; {tls}",
            count(plan.network.build_hosts.len(), "host", "hosts"),
            plan.network.run_hosts.len()
        ),
    );
    if plan.network.merged_for_whole_life && !plan.network.build_hosts.is_empty() {
        say!(
            deps,
            "{}build hosts stay reachable after the build, because the sandbox's rules cannot be changed once it exists",
            " ".repeat(LABEL)
        );
    }

    if plan.secrets.is_empty() {
        field(deps, "Secrets", "none assigned");
    } else {
        for secret in &plan.secrets {
            let route = match secret.route {
                proto::SecretRoute::Proxy => "held by the proxy",
                proto::SecretRoute::Injected => "in the environment",
                proto::SecretRoute::InjectedFile => "a file at /run/secrets",
                proto::SecretRoute::BuildSecret => "a build secret",
            };
            field(
                deps,
                "Secret",
                &format!(
                    "{} -> {} ({route})",
                    esc(&secret.name),
                    secret
                        .services
                        .iter()
                        .map(|s| esc(s))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        }
    }
    show_exposure(
        &proto::held_from_plan(&plan.secrets),
        Some(&proto::reachable_while_running(&plan.network)),
        deps,
    );

    if plan.ports.is_empty() {
        field(deps, "Ports", "none published");
    } else {
        say!(deps);
        say!(deps, "{}", paint(deps.color, Tone::Strong, "Ports it asks to publish"));
        for port in &plan.ports {
            let note = if port.was_all_interfaces {
                " (the project asked for every interface; only 127.0.0.1 is used)"
            } else {
                ""
            };
            let guess = if port.dynamic {
                " [a variable can change this]"
            } else {
                ""
            };
            let moved = if port.on_mac() == port.host_port {
                String::new()
            } else {
                format!(" (the project publishes {}; moved with {} ports)", port.host_port, brand::CLI_BIN)
            };
            say!(
                deps,
                "  {} -> {} in {}{note}{guess}{moved}",
                port.on_mac(),
                port.container_port,
                esc(&port.service)
            );
        }
    }

    let high: Vec<&proto::Finding> = plan
        .findings
        .iter()
        .filter(|f| f.severity == "Critical" || f.severity == "High")
        .collect();
    if !high.is_empty() {
        say!(deps);
        say!(
            deps,
            "{}",
            paint(deps.color, Tone::Warn, "What the project asks for")
        );
        for finding in high.iter().take(20) {
            say!(
                deps,
                "  {:<9}{}{}",
                esc(&finding.severity),
                finding
                    .service
                    .as_deref()
                    .map(|s| format!("{}: ", esc(s)))
                    .unwrap_or_default(),
                esc(&finding.message)
            );
        }
        if high.len() > 20 {
            say!(deps, "  ... and {} more. Run check for all of them.", high.len() - 20);
        }
    }
}

pub(crate) fn describe_resources(resources: &proto::ResourcePlan) -> String {
    let build = if resources.build_memory_bytes != resources.memory_bytes {
        format!(", {} while building", human_bytes(resources.build_memory_bytes))
    } else {
        String::new()
    };
    let clamped = if resources.clamped {
        " (lowered to your maximum)"
    } else {
        ""
    };
    format!(
        "{} memory{build}, {}, {} disk{clamped}",
        human_bytes(resources.memory_bytes),
        proto::cpus_text(resources.millicpus),
        human_bytes(resources.disk_bytes)
    )
}

fn show_services(services: &[proto::ServiceState], deps: &mut Deps) {
    for service in services {
        let health = service
            .health
            .as_deref()
            .map(|h| format!(" ({})", esc(h)))
            .unwrap_or_default();
        field(
            deps,
            "Service",
            &format!("{} {}{health}", esc(&service.name), esc(&service.state)),
        );
    }
}

/// Decides which ports to forward. Nothing is forwarded without a person
/// saying so, and a port the plan does not list cannot be chosen at all.
fn choose_ports(plan: &PlanReport, args: &UpArgsCli, deps: &mut Deps) -> Result<Vec<u16>, i32> {
    let available: Vec<u16> = plan.ports.iter().map(|p| p.host_port).collect();
    if available.is_empty() {
        return Ok(Vec::new());
    }
    if args.all_ports {
        return Ok(available);
    }
    if !args.ports.is_empty() {
        return listed_ports(plan, &args.ports, "up", deps);
    }
    // A choice made before, at a start or with `ports`, is used again. The
    // start's own prompt names every port it opens.
    if plan.ports.iter().any(|p| p.chosen.is_some()) {
        let chosen: Vec<&proto::PortPlan> = plan.ports.iter().filter(|p| p.chosen == Some(true)).collect();
        say!(deps);
        if chosen.is_empty() {
            say!(deps, "No ports will be forwarded, as chosen before. Change them with: {} ports", brand::CLI_BIN);
        } else {
            say!(
                deps,
                "Forwarding {} to 127.0.0.1, as chosen before. Change them with: {} ports",
                chosen.iter().map(|p| p.on_mac().to_string()).collect::<Vec<_>>().join(", "),
                brand::CLI_BIN
            );
        }
        return Ok(chosen.iter().map(|p| p.host_port).collect());
    }

    say!(deps);
    say!(
        deps,
        "Forward {} to 127.0.0.1? Anything on this machine could then reach them. [y/N] ",
        available
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut answer = String::new();
    if deps.input.read_line(&mut answer).is_err() {
        tell!(deps, "up: cannot read an answer.");
        return Err(exit::FAILED);
    }
    if answer.trim().eq_ignore_ascii_case("y") {
        Ok(available)
    } else {
        say!(deps, "No ports will be forwarded. Use --port to pick some.");
        Ok(Vec::new())
    }
}

/// Ports a person named, checked against the plan. A port the plan does not
/// list cannot be chosen at all.
pub(crate) fn listed_ports(plan: &PlanReport, asked: &[u16], command: &str, deps: &mut Deps) -> Result<Vec<u16>, i32> {
    let available: Vec<u16> = plan.ports.iter().map(|p| p.host_port).collect();
    let unknown: Vec<String> = asked
        .iter()
        .filter(|p| !available.contains(p))
        .map(u16::to_string)
        .collect();
    if !unknown.is_empty() {
        let publishes = if available.is_empty() {
            "nothing".to_owned()
        } else {
            available
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        tell!(
            deps,
            "{command}: this project does not publish {}. It publishes {publishes}.",
            unknown.join(", ")
        );
        return Err(exit::USAGE);
    }
    Ok(asked.to_vec())
}

/// Asks the daemon to fetch a remote project on this machine, with this
/// shell's SSH agent, and hands back what it reports. Prints only failures.
pub(crate) fn fetch_report(
    url: &str,
    git_ref: Option<&str>,
    command: &str,
    deps: &mut Deps,
    step: &mut dyn FnMut(&mut Deps, &str, Option<&str>),
) -> Option<proto::FetchReport> {
    if url.starts_with("http://") || url.starts_with("https://") {
        tell!(deps, "{command}: only SSH remotes can be fetched for now, such as git@github.com:org/repo.git.");
        return None;
    }
    // The agent this shell uses, which may be newer than the daemon's. It is
    // a path, not a key, and the daemon checks it belongs to this user.
    let ssh_auth_sock = (deps.env)("SSH_AUTH_SOCK")
        .map(|path| path.to_string_lossy().into_owned())
        .filter(|path| !path.is_empty());
    let answer = call_daemon(
        deps,
        Request::Fetch(proto::FetchArgs {
            url: url.to_owned(),
            git_ref: git_ref.map(str::to_owned),
            fetch: true,
            ssh_auth_sock,
        }),
        command,
        step,
    );
    match answer {
        Ok(Reply::Fetch(report)) => Some(report),
        Ok(_) => {
            unexpected(command, deps);
            None
        }
        Err(_) => None,
    }
}

/// Has the daemon fetch a remote project on this machine, and says what it
/// got. Returns the folder the daemon keeps it in.
fn fetch(url: &str, git_ref: Option<&str>, deps: &mut Deps) -> Option<PathBuf> {
    let report = fetch_report(url, git_ref, "up", deps, &mut dots)?;
    let commit = report.commit.as_deref().unwrap_or("an unknown commit");
    if report.fetched {
        say!(deps, "Fetched {} ({}) at {} on this machine, with your SSH key.", esc(&report.url), esc(&report.git_ref), esc(commit));
        say!(deps, "The sandbox gets the files. Your key, agent and Git credentials stay on this machine.");
    } else {
        say!(deps, "{} is already in a sandbox, so it was not fetched again.", esc(&report.url));
        if git_ref.is_some() {
            say!(deps, "--ref was not used for the same reason. Switch branches inside the sandbox with git.");
        }
        say!(deps, "Bring in newer commits with: {} git pull --project {}", brand::CLI_BIN, esc(url));
    }
    say!(deps, "Other commands take the same remote in place of a folder.");
    say!(deps);
    Some(PathBuf::from(report.project_path))
}

pub(crate) fn resolve(path: Option<&std::path::Path>, command: &str, deps: &mut Deps) -> Option<PathBuf> {
    match project_dir(path, deps) {
        Ok(root) => Some(root),
        Err(reason) => {
            tell!(deps, "{command}: {reason}");
            None
        }
    }
}

/// Sends one request, printing each step as it arrives.
fn request(deps: &mut Deps, request: Request, command: &str) -> Result<Reply, i32> {
    call_daemon(deps, request, command, &mut dots)
}

/// How `up` shows a step: dimmed dots and the daemon's own words.
fn dots(deps: &mut Deps, step: &str, detail: Option<&str>) {
    if let Some(line) = proto::person_step_line(step, detail) {
        say!(deps, "{} {}", paint(deps.color, Tone::Warn, "?"), line);
        return;
    }
    let line = match detail {
        Some(detail) => format!("{step}: {detail}"),
        None => step.to_owned(),
    };
    say!(deps, "{}  {}", paint(deps.color, Tone::Dim, "..."), esc(&line));
}

/// Sends one request, handing each step to `step` as it arrives. A failure
/// is printed here, so the caller only has to return the exit code.
///
/// The client reference is taken out of `deps` first, so that steps can be
/// written to the output while the call is still running.
pub(crate) fn call_daemon(
    deps: &mut Deps,
    request: Request,
    command: &str,
    step: &mut dyn FnMut(&mut Deps, &str, Option<&str>),
) -> Result<Reply, i32> {
    let client = deps.daemon;
    let result = client.call(request, &mut |name, detail| step(deps, name, detail));
    result.map_err(|error| {
        tell!(deps, "{command}: {}", error.for_terminal());
        exit::FAILED
    })
}

pub(crate) fn unexpected(command: &str, deps: &mut Deps) -> i32 {
    tell!(deps, "{command}: the daemon answered something else.");
    exit::FAILED
}

fn field(deps: &mut Deps, label: &str, value: &str) {
    say!(deps, "{}{value}", cell(deps.color, Tone::Strong, label, LABEL));
}

pub(crate) fn short(revision: &str) -> String {
    esc(revision.get(..12).unwrap_or(revision))
}

/// "1 service", "2 services".
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}
