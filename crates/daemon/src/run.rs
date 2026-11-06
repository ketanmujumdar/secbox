//! Starting, stopping and reporting on a project's sandbox.
//!
//! The order of operations follows what the engine spike actually observed on
//! a real VM, in `docs/spikes/0001-engine-compose.md`. Two of those
//! observations shape this file:
//!
//! - The Docker daemon inside the guest does not start by itself, and after a
//!   restart it refuses to start until a stale pid file is removed.
//! - Stopping the sandbox is not a clean shutdown for the containers inside
//!   it: Postgres ran crash recovery afterwards. So the workload is stopped
//!   first, and only then the sandbox.
//!
//! Everything a sandbox prints is treated as text from a compromised machine:
//! escaped, cut short, and never used to decide anything.

use std::time::{Duration, Instant};

use engine::{
    CreateSpec, ExecRequest, GuestPath, HostPort, NetworkPolicy, Resources, SandboxStatus, TlsMode,
};
use proto::{
    text, ApiError, DownReport, PlanReport, SandboxState, ServiceState, StatusReport, TlsPlan,
    UpArgs, UpReport,
};

use projectcheck::detect::{COMPOSE_FILES, COMPOSE_OVERRIDE_FILES};

use crate::secret::{self, Delivered};
use crate::settings::{Approvals, SandboxRecord};
use crate::{
    audit, bad, engine_error, internal, plan, refused, wrong_state, Progress, Project, Service,
};

/// The guest image. The spike ran this one, and it already carries Compose.
const IMAGE: &str = "docker:dind";
/// Where the project's files live inside the guest.
pub(crate) const WORK_DIR: &str = "/work";
/// Where the Compose file for a project with only a Dockerfile goes: the root
/// of the host's snapshot copy, written after the snapshot is taken and before
/// it is uploaded, so it lands at `/work/compose.yaml` in the guest.
///
/// Why there, and not on the guest's `tmpfs` with the secret override:
///
/// - `ps`, `stop`, `logs` and `exec` run `docker compose` in the work folder
///   and let Compose find its file by name, as they do for a Compose project.
///   A file anywhere else would leave those with no project to talk to, and
///   would make the build context and project name differ from a Compose
///   project's.
/// - The snapshot copy is in the daemon's own staging folder, made fresh for
///   this start, before anything has run. Writing there never touches the
///   engine's guest state, so no path the guest could have made is followed.
///   The file is created new, so a link or file already at that name, from a
///   project or a commit that has its own Compose file after all, makes the
///   start fail rather than be written through or replaced.
///
/// Once in the guest the file can be changed by the guest, exactly like a
/// Compose project's own files. That changes nothing the host relies on: the
/// approved ports and the network policy are fixed on the sandbox itself.
pub const GENERATED_COMPOSE_FILE: &str = "compose.yaml";
/// How long to wait for the Docker daemon inside the guest. The spike saw it
/// ready in 2.2 to 3.1 seconds.
const DOCKER_READY_LIMIT: Duration = Duration::from_secs(90);
const DOCKER_POLL_GAP: Duration = Duration::from_millis(500);
/// A cold build of the spike's test project took 83 to 104 seconds. This
/// leaves room for a slower machine and a cold image cache.
const BUILD_LIMIT: Duration = Duration::from_secs(25 * 60);
/// Given to Compose, so it reports an unhealthy service rather than hanging.
const COMPOSE_WAIT_SECONDS: u32 = 300;
const STOP_LIMIT: Duration = Duration::from_secs(120);
const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

pub fn up(
    service: &Service,
    project: &Project,
    args: &UpArgs,
    progress: &mut dyn Progress,
) -> Result<UpReport, ApiError> {
    let started = Instant::now();

    progress.step("planning", None);
    let plan = plan::build(service, project, &args.source)?;

    // The approval names one exact plan. If the project's files changed since
    // it was shown, this is no longer the thing the person agreed to.
    if plan.digest != args.plan_digest {
        service.record(
            audit::entry(service.deps().now_unix(), audit::Action::RequestRefused)
                .project(project.id.as_str())
                .detail("the plan changed after it was approved"),
        );
        return Err(refused(
            "the project changed after this plan was approved. Look at it again before starting.",
        ));
    }
    if !plan.blockers.is_empty() {
        return Err(refused(format!(
            "this project cannot be started yet: {}",
            plan.blockers.join(" / ")
        )));
    }

    let state = status_of(service, project)?;
    let ports = approved_ports(service, project, &plan, args, state)?;

    progress.step("sandbox", None);
    let fresh = matches!(state, SandboxState::None);
    match state {
        SandboxState::None => create(service, project, &plan, &ports, args)?,
        SandboxState::Stopped => {
            service
                .deps()
                .engine()
                .start(&project.sandbox)
                .map_err(|e| engine_error(e.to_string()))?;
            service.record(
                audit::entry(service.deps().now_unix(), audit::Action::SandboxStarted)
                    .project(project.id.as_str()),
            );
        }
        SandboxState::Running => {}
        SandboxState::Unknown => {
            return Err(wrong_state(
                "the sandbox is in a state this version does not understand. Stop it and try again.",
            ))
        }
    }

    if fresh {
        progress.step("snapshot", None);
        if let Err(error) = copy_in_snapshot(service, project, &plan, args) {
            // Code is copied only into a new sandbox, so one left behind here
            // would be reused empty by the next start. It holds nothing yet.
            let _ = service.deps().engine().destroy(&project.sandbox);
            let _ = SandboxRecord::remove(&project.dir);
            return Err(error);
        }
    }

    progress.step("docker", None);
    start_docker(service, project)?;

    // Secrets live on a temporary filesystem inside the guest, so a sandbox
    // that was stopped and started again has none. They go in on every start.
    progress.step("secrets", None);
    let delivered = secret::deliver(service, project)?;

    progress.step("build", None);
    compose_up(service, project, &plan, &delivered)?;

    let services = compose_services(service, project)?;
    service.record(
        audit::entry(service.deps().now_unix(), audit::Action::BuildFinished)
            .project(project.id.as_str()),
    );

    Ok(UpReport {
        project_id: project.id.as_str().to_owned(),
        sandbox: SandboxState::Running,
        services,
        ports,
        seconds: started.elapsed().as_secs(),
        secrets: delivered
            .by_service
            .iter()
            .map(|(service, names)| proto::SecretsDelivered {
                service: service.clone(),
                names: names.clone(),
            })
            .collect(),
    })
}

/// Checks the ports a person approved against the daemon's own plan. A port
/// that is not in the plan is refused, so a client cannot open one by asking.
fn approved_ports(
    service: &Service,
    project: &Project,
    plan: &PlanReport,
    args: &UpArgs,
    state: SandboxState,
) -> Result<Vec<u16>, ApiError> {
    let mut approved: Vec<u16> = Vec::new();
    for port in &args.approve_ports {
        if !plan.ports.iter().any(|p| p.host_port == *port) {
            service.record(
                audit::entry(service.deps().now_unix(), audit::Action::PortRefused)
                    .project(project.id.as_str())
                    .subject(port.to_string())
                    .detail("not in the plan"),
            );
            return Err(bad(format!(
                "port {port} is not one this project publishes, so it cannot be approved"
            )));
        }
        if !approved.contains(port) {
            approved.push(*port);
        }
    }
    approved.sort_unstable();
    // Ports are fixed when a sandbox is created, and the engine cannot add the
    // rule that lets one in afterwards. Saying a port was forwarded when nothing
    // changed would be worse than refusing.
    if !matches!(state, SandboxState::None) {
        if let Some(record) = SandboxRecord::load(&project.dir).map_err(internal)? {
            let mut existing = record.ports.clone();
            existing.sort_unstable();
            if existing != approved {
                let open = if existing.is_empty() {
                    "no ports".to_owned()
                } else {
                    existing.iter().map(u16::to_string).collect::<Vec<_>>().join(", ")
                };
                return Err(wrong_state(format!(
                    "this sandbox already exists with {open} forwarded, and ports can only be set when a sandbox is created. To change them, run: {} down --destroy, which deletes its data, then start it again",
                    brand::CLI_BIN
                )));
            }
        }
    }
    for port in &approved {
        service.record(
            audit::entry(service.deps().now_unix(), audit::Action::PortForwarded)
                .project(project.id.as_str())
                .subject(port.to_string()),
        );
    }
    for planned in &plan.ports {
        if !approved.contains(&planned.host_port) {
            service.record(
                audit::entry(service.deps().now_unix(), audit::Action::PortRefused)
                    .project(project.id.as_str())
                    .subject(planned.host_port.to_string())
                    .detail("not approved"),
            );
        }
    }
    let mut saved = Approvals::load(&project.dir).map_err(internal)?;
    saved.ports = approved.clone();
    saved.save(&project.dir).map_err(internal)?;
    Ok(approved)
}

fn create(
    service: &Service,
    project: &Project,
    plan: &PlanReport,
    ports: &[u16],
    args: &UpArgs,
) -> Result<(), ApiError> {
    // One policy has to cover the sandbox's whole life: the spike did not
    // establish that it can change between build and run, so the build hosts
    // stay reachable afterwards. The plan says so, and the person approved it.
    let mut hosts: Vec<String> = plan.network.build_hosts.clone();
    for host in &plan.network.run_hosts {
        if !hosts.contains(host) {
            hosts.push(host.clone());
        }
    }
    let tls = match plan.network.tls {
        TlsPlan::ServerNameOnly => TlsMode::ServerNameOnly,
        TlsPlan::Intercept => {
            return Err(crate::unsupported(
                "this plan needs the gateway to terminate TLS, which is not settled yet",
            ))
        }
    };

    // Memory cannot be raised later either, so the sandbox is created at the
    // larger of the run and build limits.
    let peak = plan.resources.memory_bytes.max(plan.resources.build_memory_bytes);
    let resources = Resources::new(
        u32::try_from(peak / MIB).map_err(|_| bad("the memory limit is too large"))?,
        (plan.resources.millicpus.div_ceil(1000)).max(1),
        u32::try_from((plan.resources.disk_bytes / GIB).max(1))
            .map_err(|_| bad("the disk limit is too large"))?,
    )
    .map_err(|e| bad(e.to_string()))?;

    let spec = CreateSpec {
        name: project.sandbox.clone(),
        image: IMAGE.to_owned(),
        resources,
        network: if hosts.is_empty() {
            NetworkPolicy::DenyAll
        } else {
            NetworkPolicy::AllowHosts { hosts, tls }
        },
        // Proxy-held secrets need TLS interception, which the plan refuses
        // while that question is open, so nothing is bound here yet.
        secrets: Vec::new(),
        ports: ports.iter().map(|p| HostPort(*p)).collect(),
    };
    service
        .deps()
        .engine()
        .create(&spec)
        .map_err(|e| engine_error(e.to_string()))?;

    SandboxRecord {
        version: 1,
        created_unix: service.deps().now_unix(),
        memory_bytes: plan.resources.memory_bytes,
        millicpus: plan.resources.millicpus,
        disk_bytes: plan.resources.disk_bytes,
        build_memory_bytes: plan.resources.build_memory_bytes,
        ports: ports.to_vec(),
        plan_digest: args.plan_digest.clone(),
        log_level: Some(crate::blocked::created_level()),
        reachable_hosts: Some(proto::reachable_while_running(&plan.network)),
        // Set once the snapshot is in, by the copy that makes it.
        baseline: None,
    }
    .save(&project.dir)
    .map_err(internal)?;

    service.record(
        audit::entry(service.deps().now_unix(), audit::Action::SandboxCreated)
            .project(project.id.as_str())
            .detail(format!("{} hosts allowed", spec_host_count(&spec))),
    );
    Ok(())
}

fn spec_host_count(spec: &CreateSpec) -> usize {
    match &spec.network {
        NetworkPolicy::AllowHosts { hosts, .. } => hosts.len(),
        _ => 0,
    }
}

fn copy_in_snapshot(
    service: &Service,
    project: &Project,
    plan: &PlanReport,
    args: &UpArgs,
) -> Result<(), ApiError> {
    let staging = tempdir_in_state(service)?;
    // A project imported from a remote becomes a real clone, through the Git
    // relay, at the commit the plan was made from. Its history comes along.
    if let Some(remote) = crate::remote::remote_of(service, project) {
        let commit = crate::gitref::head_commit(&project.root)
            .map_err(|e| internal(format!("cannot tell which commit was approved: {e}")))?;
        crate::diff::forget_baseline(project);
        crate::gitrelay::clone_into_sandbox(service, project, &remote.url, &remote.git_ref, &commit)?;
        // The approved commit is what `diff` compares with. The host's mirror
        // holds it, so nothing is read back from the clone in the guest.
        crate::diff::remember_baseline(project, Some(commit.clone()))?;
        if let Some(generated) = &plan.generated_compose {
            let file = staging.path().join(GENERATED_COMPOSE_FILE);
            std::fs::write(&file, generated)
                .map_err(|e| internal(format!("cannot write the generated Compose file: {}", e.kind())))?;
            let guest = GuestPath::new(format!("{WORK_DIR}/{GENERATED_COMPOSE_FILE}"))
                .map_err(|e| internal(e.to_string()))?;
            service
                .deps()
                .engine()
                .copy_in(&project.sandbox, &file, &guest)
                .map_err(|e| engine_error(e.to_string()))?;
        }
        service.record(
            audit::entry(service.deps().now_unix(), audit::Action::SnapshotTaken)
                .project(project.id.as_str())
                .detail(format!("cloned {} at {commit}", remote.url)),
        );
        return Ok(());
    }
    // The snapshot creates its own destination and refuses one that exists.
    let snapshot = staging.path().join("snapshot");
    let report = service
        .deps()
        .snapshot(&project.root, &args.source, &snapshot)
        .map_err(|reason| internal(format!("cannot take a snapshot: {reason}")))?;
    for excluded in &report.excluded {
        service.record(
            audit::entry(service.deps().now_unix(), audit::Action::SnapshotExcluded)
                .project(project.id.as_str())
                .subject(&excluded.path)
                .detail(&excluded.reason),
        );
    }
    if let Some(generated) = &plan.generated_compose {
        write_generated_compose(&snapshot, generated)?;
    }
    // Last, so the commit holds exactly what the guest is given.
    let baseline = crate::diff::make_baseline(service, project, &snapshot);
    let guest = GuestPath::new(WORK_DIR).map_err(|e| internal(e.to_string()))?;
    service
        .deps()
        .engine()
        .copy_in(&project.sandbox, &snapshot, &guest)
        .map_err(|e| engine_error(e.to_string()))?;
    crate::diff::remember_baseline(project, baseline)?;
    service.record(
        audit::entry(service.deps().now_unix(), audit::Action::SnapshotTaken)
            .project(project.id.as_str())
            .detail(match &report.counts {
                Some(counts) => format!("{} files", counts.files),
                None => "count unknown".to_owned(),
            }),
    );
    Ok(())
}

/// Puts the generated Compose file into the snapshot copy. See
/// [`GENERATED_COMPOSE_FILE`] for why it goes here.
fn write_generated_compose(snapshot: &std::path::Path, generated: &str) -> Result<(), ApiError> {
    use std::io::Write;
    // The plan was made from the working tree, but the snapshot can be of a
    // commit. If that has a Compose file of its own, the plan is not about it.
    for name in COMPOSE_FILES.iter().chain(COMPOSE_OVERRIDE_FILES.iter()) {
        if std::fs::symlink_metadata(snapshot.join(name)).is_ok() {
            return Err(refused(format!(
                "the snapshot has its own {name}, but the plan was made for a project with only a Dockerfile. Look at the plan again."
            )));
        }
    }
    // Created new: an existing file or link at this name is an error, never
    // written through.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(snapshot.join(GENERATED_COMPOSE_FILE))
        .map_err(|e| internal(format!("cannot write the generated Compose file: {}", e.kind())))?;
    file.write_all(generated.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|e| internal(format!("cannot write the generated Compose file: {}", e.kind())))
}

pub(crate) fn tempdir_in_state(service: &Service) -> Result<tempfile::TempDir, ApiError> {
    let staging = service.state_dir().join("staging");
    hoststate::ensure_private_dir(&staging).map_err(internal)?;
    tempfile::tempdir_in(&staging)
        .map_err(|e| internal(format!("cannot make a staging folder: {}", e.kind())))
}

/// Starts the Docker daemon inside the guest and waits for it to answer.
fn start_docker(service: &Service, project: &Project) -> Result<(), ApiError> {
    let engine = service.deps().engine();
    if docker_ready(service, project) {
        return Ok(());
    }
    // After a restart the daemon refuses to start while a pid file from the
    // previous life is still there. Removing it is safe: nothing in this
    // sandbox is running yet.
    let _ = engine.exec(
        &project.sandbox,
        &ExecRequest::new(["sh", "-c", "rm -f /var/run/docker.pid"]),
    );
    engine
        .exec_detached(
            &project.sandbox,
            &ExecRequest::new(["sh", "-c", "dockerd >/tmp/dockerd.log 2>&1"]),
        )
        .map_err(|e| engine_error(e.to_string()))?;

    let deadline = Instant::now() + DOCKER_READY_LIMIT;
    while Instant::now() < deadline {
        if docker_ready(service, project) {
            return Ok(());
        }
        std::thread::sleep(DOCKER_POLL_GAP);
    }
    Err(engine_error(
        "the Docker daemon inside the sandbox did not start",
    ))
}

fn docker_ready(service: &Service, project: &Project) -> bool {
    service
        .deps()
        .engine()
        .exec(&project.sandbox, &ExecRequest::new(["docker", "info"]))
        .is_ok_and(|out| out.code == 0)
}

/// The start of every Compose command: the project's own files in the order
/// Compose would merge them, then the host's override when there is one.
///
/// Naming the files explicitly is what makes room for the override. The
/// project's own override files are listed too, so this reads exactly what
/// the project would read on its own.
pub(crate) fn compose_command(plan: &PlanReport, delivered: &Delivered) -> Vec<String> {
    let mut args = vec!["docker".to_owned(), "compose".to_owned()];
    for file in &plan.compose_files {
        args.push("-f".to_owned());
        args.push(file.clone());
    }
    if let Some(override_file) = &delivered.override_file {
        args.push("-f".to_owned());
        args.push(override_file.clone());
    }
    args
}

fn compose_up(
    service: &Service,
    project: &Project,
    plan: &PlanReport,
    delivered: &Delivered,
) -> Result<(), ApiError> {
    service.record(
        audit::entry(service.deps().now_unix(), audit::Action::BuildStarted)
            .project(project.id.as_str()),
    );
    let mut command = compose_command(plan, delivered);
    command.extend(
        [
            "up",
            "--build",
            "-d",
            "--wait",
            "--wait-timeout",
            &COMPOSE_WAIT_SECONDS.to_string(),
        ]
        .map(str::to_owned),
    );
    let mut request = ExecRequest::new(command);
    request.timeout = Some(BUILD_LIMIT);
    request.workdir = Some(GuestPath::new(WORK_DIR).map_err(|e| internal(e.to_string()))?);
    let out = service
        .deps()
        .engine()
        .exec(&project.sandbox, &request)
        .map_err(|e| engine_error(e.to_string()))?;
    if out.code == 0 {
        return Ok(());
    }
    Err(engine_error(format!(
        "the project did not start inside the sandbox: {}",
        text::escape_short(out.stderr.trim(), 1500)
    )))
}

/// Asks Compose what is running. The answer comes from inside the sandbox, so
/// it is shown and never believed.
fn compose_services(service: &Service, project: &Project) -> Result<Vec<ServiceState>, ApiError> {
    let mut request = ExecRequest::new(["docker", "compose", "ps", "-a", "--format", "json"]);
    request.workdir = Some(GuestPath::new(WORK_DIR).map_err(|e| internal(e.to_string()))?);
    let Ok(out) = service.deps().engine().exec(&project.sandbox, &request) else {
        return Ok(Vec::new());
    };
    Ok(parse_compose_ps(&out.stdout))
}

/// Compose writes one object per line. A line this does not understand is
/// skipped rather than guessed at.
pub(crate) fn parse_compose_ps(stdout: &str) -> Vec<ServiceState> {
    let mut states = Vec::new();
    for line in stdout.lines().take(500) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        for item in value.as_array().map_or_else(
            || vec![value.clone()],
            |items| items.iter().take(500).cloned().collect(),
        ) {
            let field = |key: &str| {
                item.get(key)
                    .and_then(serde_json::Value::as_str)
                    .map(|s| text::escape_short(s, 120))
            };
            let Some(name) = field("Service").or_else(|| field("Name")) else {
                continue;
            };
            states.push(ServiceState {
                name,
                state: field("State").unwrap_or_else(|| "unknown".to_owned()),
                health: field("Health").filter(|h| !h.is_empty()),
            });
        }
    }
    states
}

pub fn down(
    service: &Service,
    project: &Project,
    destroy: bool,
    progress: &mut dyn Progress,
) -> Result<DownReport, ApiError> {
    let engine = service.deps().engine();
    let state = status_of(service, project)?;
    if matches!(state, SandboxState::None) {
        return Ok(DownReport {
            project_id: project.id.as_str().to_owned(),
            sandbox: SandboxState::None,
            destroyed: false,
        });
    }

    if matches!(state, SandboxState::Running) {
        // Stopping the sandbox is abrupt for what runs inside it, so the
        // workload gets a chance to shut down first.
        progress.step("stopping services", None);
        let mut request = ExecRequest::new(["docker", "compose", "stop", "-t", "30"]);
        request.timeout = Some(STOP_LIMIT);
        request.workdir = GuestPath::new(WORK_DIR).ok();
        let _ = engine.exec(&project.sandbox, &request);
    }

    if destroy {
        progress.step("destroying", None);
        engine
            .destroy(&project.sandbox)
            .map_err(|e| engine_error(e.to_string()))?;
        SandboxRecord::remove(&project.dir).map_err(internal)?;
        crate::diff::forget_baseline(project);
        service.record(
            audit::entry(service.deps().now_unix(), audit::Action::SandboxDestroyed)
                .project(project.id.as_str()),
        );
        return Ok(DownReport {
            project_id: project.id.as_str().to_owned(),
            sandbox: SandboxState::None,
            destroyed: true,
        });
    }

    progress.step("stopping", None);
    engine
        .stop(&project.sandbox)
        .map_err(|e| engine_error(e.to_string()))?;
    service.record(
        audit::entry(service.deps().now_unix(), audit::Action::SandboxStopped)
            .project(project.id.as_str()),
    );
    Ok(DownReport {
        project_id: project.id.as_str().to_owned(),
        sandbox: SandboxState::Stopped,
        destroyed: false,
    })
}

pub fn status(service: &Service, project: &Project) -> Result<StatusReport, ApiError> {
    let state = status_of(service, project)?;
    let record = SandboxRecord::load(&project.dir).map_err(internal)?;
    let services = if matches!(state, SandboxState::Running) {
        compose_services(service, project)?
    } else {
        Vec::new()
    };
    let blocked = crate::blocked::summary(service, project, state);
    Ok(StatusReport {
        project_id: project.id.as_str().to_owned(),
        project_path: text::escape_path(&project.root),
        sandbox: state,
        services,
        ports: record.as_ref().map(|r| r.ports.clone()).unwrap_or_default(),
        resources: record.as_ref().map(|r| proto::ResourcePlan {
            memory_bytes: r.memory_bytes,
            millicpus: r.millicpus,
            disk_bytes: r.disk_bytes,
            build_memory_bytes: r.build_memory_bytes,
            clamped: false,
        }),
        // Not "no requests were blocked", even at zero: only hostname lookups
        // are ever recorded, and only by a sandbox created to record them.
        blocked_count: blocked.total,
        blocked: Some(blocked),
        exposure: Some(Box::new(proto::SecretExposure {
            secrets: held_secrets(project)?,
            reachable_hosts: record.as_ref().and_then(|r| r.reachable_hosts.clone()),
        })),
    })
}

/// The secrets assigned to this project, by name, and who gets each.
fn held_secrets(project: &Project) -> Result<Vec<proto::HeldSecret>, ApiError> {
    let Some(meta) = hoststate::read_meta(&project.dir).map_err(internal)? else {
        return Ok(Vec::new());
    };
    Ok(meta
        .assignments
        .iter()
        .map(|a| proto::HeldSecret {
            name: text::escape(a.name().as_str()),
            services: a.services().iter().map(|s| text::escape(s.as_str())).collect(),
        })
        .collect())
}

pub(crate) fn status_of(service: &Service, project: &Project) -> Result<SandboxState, ApiError> {
    match service.deps().engine().status(&project.sandbox) {
        Ok(SandboxStatus::NotFound) => Ok(SandboxState::None),
        Ok(SandboxStatus::Stopped) => Ok(SandboxState::Stopped),
        Ok(SandboxStatus::Running) => Ok(SandboxState::Running),
        Ok(SandboxStatus::Other(_)) => Ok(SandboxState::Unknown),
        Err(error) => Err(engine_error(error.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_output_is_read_one_object_per_line() {
        let out = r#"{"Service":"api","State":"running","Health":"healthy"}
{"Service":"web","State":"running","Health":""}"#;
        let states = parse_compose_ps(out);
        assert_eq!(states.len(), 2);
        assert_eq!(states[0].name, "api");
        assert_eq!(states[0].health.as_deref(), Some("healthy"));
        assert_eq!(states[1].health, None);
    }

    #[test]
    fn a_json_array_is_read_too() {
        let states = parse_compose_ps(r#"[{"Service":"a","State":"running"}]"#);
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].state, "running");
    }

    #[test]
    fn a_line_that_makes_no_sense_is_skipped_not_guessed_at() {
        let states = parse_compose_ps("not json\n{\"State\":\"running\"}\n{\"Service\":\"a\"}");
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].name, "a");
        assert_eq!(states[0].state, "unknown");
    }

    /// The sandbox chooses this text, so it must not be able to send control
    /// sequences to whoever reads the report.
    #[test]
    fn a_hostile_service_name_is_escaped() {
        let states = parse_compose_ps(r#"{"Service":"a\u001b[31mb","State":"running"}"#);
        assert_eq!(states.len(), 1);
        assert!(!states[0].name.contains('\u{1b}'));
    }

    #[test]
    fn enormous_output_does_not_become_an_enormous_report() {
        let line = r#"{"Service":"a","State":"running"}"#;
        let out = std::iter::repeat_n(line, 5000).collect::<Vec<_>>().join("\n");
        assert!(parse_compose_ps(&out).len() <= 500);
    }
}
