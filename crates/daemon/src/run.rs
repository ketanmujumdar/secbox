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
    CreateSpec, ExecRequest, GuestPath, NetworkPolicy, PortForward, ResizeSpec, Resources, SandboxStatus, TlsMode,
};
use proto::{
    text, ApiError, DownReport, PlanReport, RemoveReport, SandboxState, ServiceState, StatusReport, TlsPlan,
    UpArgs, UpReport,
};

use projectcheck::detect::{COMPOSE_FILES, COMPOSE_OVERRIDE_FILES};

use crate::secret::{self, Delivered};
use crate::settings::{Approvals, SandboxRecord};
use crate::{
    audit, bad, engine_error, internal, person, plan, refused, wrong_state, Progress, Project, Service,
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

/// Runs in the guest before `dockerd`, on every start of the VM.
///
/// Docker writes each container's whole environment, secrets included, to
/// three places: `/var/lib/docker/containers/<id>/config.v2.json`, the runtime
/// bundle under `/run/docker`, and the container's spec in containerd's
/// metadata database. All are on the VM's root disk, which is a file on this
/// machine (measured on msb 0.7.13: `/run` is not a tmpfs). So they are kept
/// in memory. Containers are made again by `compose up` on each start; images
/// and volumes, the project's data, stay on disk.
///
/// containerd's database also holds the images in Docker's default image
/// store, so `dockerd` runs with the classic store (see [`DOCKERD`]), where it
/// holds only containers. A sandbox made before this pulls or builds its
/// images again once.
///
/// What an earlier start left in those folders on disk is deleted first, there
/// and in the kept disk of a sandbox that replaced another, and the freed
/// blocks are discarded: the engine punches them out of the disk file, so an
/// old value is not left in free space. The VM has no swap, so a tmpfs page
/// never reaches a disk. If a shim is somehow running, the old records are left
/// alone and the mounts are still made.
///
/// The pid file from the previous life is removed too: the daemon refuses to
/// start while it is there.
const DOCKER_STATE_IN_MEMORY: &str = r#"
set -e
rm -f /var/run/docker.pid
idle=yes
if pidof containerd-shim-runc-v2 >/dev/null 2>&1; then idle=no; fi
# dockerd bind-mounts its root onto itself when it is not a mount point yet,
# without what is mounted below it, which hid these tmpfs mounts and sent the
# records to disk (measured). Made a mount point first, it is left as it is.
mkdir -p /var/lib/docker
if ! grep -q " /var/lib/docker " /proc/mounts; then mount --bind /var/lib/docker /var/lib/docker; fi
for d in /var/lib/docker/containers /var/lib/docker/network /var/lib/docker/containerd/daemon/io.containerd.metadata.v1.bolt /run/docker; do
  mkdir -p "$d"
  if [ "$(stat -f -c %T "$d")" = tmpfs ]; then continue; fi
  if [ "$idle" = yes ]; then find "$d" -mindepth 1 -delete; fi
  mount -t tmpfs -o mode=0710,nosuid,nodev tmpfs "$d"
  [ "$(stat -f -c %T "$d")" = tmpfs ]
done
if grep -q " /previous-disk " /proc/mounts; then
  for d in var/lib/docker/containers var/lib/docker/network var/lib/docker/containerd/daemon/io.containerd.metadata.v1.bolt run/docker; do
    if [ -d "/previous-disk/$d" ]; then find "/previous-disk/$d" -mindepth 1 -delete; fi
  done
  fstrim /previous-disk >/dev/null 2>&1 || true
fi
sync
fstrim / >/dev/null 2>&1 || true
"#;

/// The classic image store, and capped logs: container logs are in memory now,
/// with the container records.
const DOCKERD: &str = "dockerd --feature containerd-snapshotter=false --log-opt max-size=10m --log-opt max-file=2 >/tmp/dockerd.log 2>&1";

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

/// What the person is asked before a start: "start shop in its sandbox,
/// with 2 secrets, able to reach 3 hosts, opening port 3000".
fn start_reason(project: &Project, plan: &proto::PlanReport, ports: &[PortForward]) -> String {
    let mut hosts: Vec<&str> = plan.network.build_hosts.iter().chain(&plan.network.run_hosts).map(String::as_str).collect();
    hosts.sort_unstable();
    hosts.dedup();
    let secrets = match plan.secrets.len() {
        0 => "with no secrets".to_owned(),
        n => format!("with {}", person::count(n, "secret", "secrets")),
    };
    let reach = match hosts.len() {
        0 => "reaching no hosts".to_owned(),
        n => format!("able to reach {}", person::count(n, "host", "hosts")),
    };
    let named: Vec<String> = ports.iter().map(|p| crate::portset::describe(p, &plan.ports)).collect();
    let ports = match named.as_slice() {
        [] => String::new(),
        [one] => format!(", opening port {one}"),
        many => format!(", opening ports {}", many.join(" ")),
    };
    format!("start {} in its sandbox, {secrets}, {reach}{ports}", person::project_name(project))
}

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
    let approved = approved_ports(service, project, &plan, args, state)?;
    let forwards = crate::portset::wanted(&plan.ports, &approved);

    // Everything that can refuse on its own has; now the person decides.
    // Starting delivers the secrets, so this is asked on every start.
    service.ask_person(project, person::Act::Start, &start_reason(project, &plan, &forwards), progress)?;

    progress.step("sandbox", None);
    let fresh = matches!(state, SandboxState::None);
    match state {
        SandboxState::None => create(service, project, &plan, &approved, args, None)?,
        SandboxState::Stopped => {
            apply_resources(service, project, &plan.resources)?;
            crate::portset::apply(service, project, &forwards)?;
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

    Ok(UpReport {
        project_id: project.id.as_str().to_owned(),
        sandbox: SandboxState::Running,
        services,
        ports: forwards.iter().map(|p| p.host).collect(),
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
    // A running sandbox keeps its ports, and one made before ports could
    // change cannot open new ones. Saying a port was forwarded when nothing
    // changed would be worse than refusing.
    let forwards = crate::portset::wanted(&plan.ports, &approved);
    crate::portset::check_start(project, state, &forwards, &plan.ports)?;
    for forward in &forwards {
        let entry = audit::entry(service.deps().now_unix(), audit::Action::PortForwarded)
            .project(project.id.as_str())
            .subject(forward.host.to_string());
        service.record(if forward.host == forward.guest {
            entry
        } else {
            entry.detail(format!("for port {}", forward.guest))
        });
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
    saved.ports_chosen = true;
    saved.save(&project.dir).map_err(internal)?;
    Ok(approved)
}

pub(crate) fn create(
    service: &Service,
    project: &Project,
    plan: &PlanReport,
    ports: &[u16],
    args: &UpArgs,
    previous_disk: Option<std::path::PathBuf>,
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

    let resources = engine_resources(&plan.resources)?;

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
        ports: crate::portset::wanted(&plan.ports, ports),
        // Every port the plan lists, so any of them can be opened later.
        ingress_ports: plan.ports.iter().map(|p| p.host_port).collect(),
        previous_disk,
    };
    service
        .deps()
        .engine()
        .create(&spec)
        .map_err(|e| engine_error(e.to_string()))?;

    crate::blocked::new_log(project);
    SandboxRecord {
        version: 1,
        created_unix: service.deps().now_unix(),
        memory_bytes: plan.resources.memory_bytes,
        millicpus: plan.resources.millicpus,
        disk_bytes: plan.resources.disk_bytes,
        build_memory_bytes: plan.resources.build_memory_bytes,
        ports: ports.to_vec(),
        plan_digest: args.plan_digest.clone(),
        mac_ports: crate::portset::wanted(&plan.ports, ports)
            .iter()
            .filter(|p| p.host != p.guest)
            .map(|p| (p.guest, p.host))
            .collect(),
        ingress_ports: Some(plan.ports.iter().map(|p| p.host_port).collect()),
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
            .detail(format!("{} allowed", person::count(spec_host_count(&spec), "host", "hosts"))),
    );
    Ok(())
}

/// Memory cannot be raised while a sandbox runs, so its VM is sized for the
/// larger of the run and build limits.
fn engine_resources(plan: &proto::ResourcePlan) -> Result<Resources, ApiError> {
    let peak = plan.memory_bytes.max(plan.build_memory_bytes);
    Resources::new(
        u32::try_from(peak / MIB).map_err(|_| bad("the memory limit is too large"))?,
        (plan.millicpus.div_ceil(1000)).max(1),
        u32::try_from((plan.disk_bytes / GIB).max(1)).map_err(|_| bad("the disk limit is too large"))?,
    )
    .map_err(|e| bad(e.to_string()))
}

/// Brings a stopped sandbox to the limits of the plan a person just approved,
/// so that what the review showed is what it starts with.
fn apply_resources(service: &Service, project: &Project, plan: &proto::ResourcePlan) -> Result<(), ApiError> {
    // A sandbox with no record cannot be compared with anything, and has nowhere
    // to say what it was given. It keeps what it has.
    let Some(mut record) = SandboxRecord::load(&project.dir).map_err(internal)? else {
        return Ok(());
    };
    let same = record.memory_bytes == plan.memory_bytes
        && record.build_memory_bytes == plan.build_memory_bytes
        && record.millicpus == plan.millicpus
        && record.disk_bytes == plan.disk_bytes;
    if same {
        return Ok(());
    }
    let target = engine_resources(plan)?;
    let spec = ResizeSpec {
        memory_mib: target.memory_mib,
        cpus: target.cpus,
        // In the units the engine has it in, since it refuses an unchanged size.
        grow_disk_gib: (target.disk_gib > (record.disk_bytes / GIB).max(1) as u32).then_some(target.disk_gib),
    };
    service
        .deps()
        .engine()
        .resize(&project.sandbox, &spec)
        .map_err(|e| engine_error(e.to_string()))?;
    record.memory_bytes = plan.memory_bytes;
    record.build_memory_bytes = plan.build_memory_bytes;
    record.millicpus = plan.millicpus;
    record.disk_bytes = plan.disk_bytes;
    record.save(&project.dir).map_err(internal)?;
    service.record(
        audit::entry(service.deps().now_unix(), audit::Action::SandboxResized)
            .project(project.id.as_str())
            .detail(format!(
                "{} MiB memory, {} millicpus, {} MiB disk",
                plan.memory_bytes.max(plan.build_memory_bytes) / MIB,
                plan.millicpus,
                plan.disk_bytes / MIB
            )),
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
pub(crate) fn start_docker(service: &Service, project: &Project) -> Result<(), ApiError> {
    let engine = service.deps().engine();
    if docker_ready(service, project) {
        return Ok(());
    }
    let out = engine
        .exec(&project.sandbox, &ExecRequest::new(["sh", "-c", DOCKER_STATE_IN_MEMORY]))
        .map_err(|e| engine_error(e.to_string()))?;
    if out.code != 0 {
        return Err(engine_error(format!(
            "could not keep Docker's container records in memory, so nothing was started and no secret was delivered: {}",
            proto::text::escape(out.stderr.trim())
        )));
    }
    engine
        .exec_detached(
            &project.sandbox,
            &ExecRequest::new(["sh", "-c", DOCKERD]),
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
/// Compose would merge them, then the host's overrides, each when there is
/// one: the one that publishes the approved ports (see [`crate::ports`]), and
/// the secrets one.
///
/// Naming the files explicitly is what makes room for the overrides. The
/// project's own override files are listed too, so this reads exactly what
/// the project would read on its own.
pub(crate) fn compose_command(
    plan: &PlanReport,
    ports_file: Option<&str>,
    delivered: &Delivered,
) -> Vec<String> {
    let mut args = vec!["docker".to_owned(), "compose".to_owned()];
    for file in &plan.compose_files {
        args.push("-f".to_owned());
        args.push(file.clone());
    }
    if let Some(ports_file) = ports_file {
        args.push("-f".to_owned());
        args.push(ports_file.to_owned());
    }
    if let Some(override_file) = &delivered.override_file {
        args.push("-f".to_owned());
        args.push(override_file.clone());
    }
    args
}

pub(crate) fn compose_up(
    service: &Service,
    project: &Project,
    plan: &PlanReport,
    delivered: &Delivered,
) -> Result<(), ApiError> {
    service.record(
        audit::entry(service.deps().now_unix(), audit::Action::BuildStarted)
            .project(project.id.as_str()),
    );
    // Written on every start, after the secrets. The engine's forward arrives
    // on the guest's own address, so a port the project published on the
    // guest's loopback alone would never answer.
    let ports_file = crate::ports::write_override(service, project, &plan.ports)?;
    let mut command = compose_command(plan, ports_file.as_deref(), delivered);
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
        service.record(
            audit::entry(service.deps().now_unix(), audit::Action::BuildFinished)
                .project(project.id.as_str()),
        );
        return Ok(());
    }
    service.record(
        audit::entry(service.deps().now_unix(), audit::Action::BuildFailed)
            .project(project.id.as_str())
            .detail(format!("Compose stopped with exit code {}", out.code)),
    );
    // A half-started project must not stay up with its ports open and its
    // secrets delivered. Stopping keeps the data, as `down` does.
    let stopped = service.deps().engine().stop(&project.sandbox).is_ok();
    if stopped {
        service.record(
            audit::entry(service.deps().now_unix(), audit::Action::SandboxStopped)
                .project(project.id.as_str()),
        );
    }
    // The output comes from the project, and a service can print a secret.
    let lines = match crate::mask::Masker::for_project(service, project) {
        Ok(masker) => text::escape_last_lines(&masker.mask(&out.stderr), BUILD_ERROR_LINES, 200),
        Err(_) => "(not shown, because the secret values to hide in it could not be read)".to_owned(),
    };
    let after = if stopped {
        "The sandbox was stopped, so its ports are closed; its data is kept. Fix the project, then start it again."
    } else {
        "The sandbox could not be stopped afterwards. Stop it yourself before trying again."
    };
    Err(engine_error(format!(
        "the project did not start inside the sandbox. {after} The last lines Compose printed:\n{lines}"
    )))
}

/// How many of Compose's last lines a failed start shows.
const BUILD_ERROR_LINES: usize = 15;

/// Asks Compose what is running. The answer comes from inside the sandbox, so
/// it is shown and never believed.
pub(crate) fn compose_services(service: &Service, project: &Project) -> Result<Vec<ServiceState>, ApiError> {
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
        crate::netallow::forget_kept_disk(project);
        crate::blocked::forget_all(project);
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

/// Removes a project from this machine. The records go last, so if anything
/// before them fails the project is still listed and this can be tried again.
///
/// `project.root` itself is never removed: only the daemon's own copy of an
/// imported project, which `imported_folder` names. Its secrets are deleted
/// from the keychain; one that cannot be is named in the report, and the
/// project is removed all the same.
pub fn remove(
    service: &Service,
    project: &Project,
    progress: &mut dyn Progress,
) -> Result<RemoveReport, ApiError> {
    let imported = crate::remote::imported_folder(service, project);
    let sandbox_destroyed = down(service, project, true, progress)?.destroyed;
    // Before the records go, since the names are in them.
    let (secrets_removed, secrets_left) = remove_secrets(service, project, progress);
    if let Some(folder) = &imported {
        progress.step("removing the imported copy", None);
        std::fs::remove_dir_all(folder)
            .map_err(|e| internal(format!("cannot remove the imported copy: {}", e.kind())))?;
    }
    progress.step("removing the project's records", None);
    std::fs::remove_dir_all(&project.dir)
        .map_err(|e| internal(format!("cannot remove the project's records: {}", e.kind())))?;
    service.record(
        audit::entry(service.deps().now_unix(), audit::Action::ProjectRemoved).project(project.id.as_str()),
    );
    Ok(RemoveReport {
        project_id: project.id.as_str().to_owned(),
        sandbox_destroyed,
        removed_copy: imported.is_some(),
        secrets_removed,
        secrets_left,
    })
}

/// Deletes every secret a project has in the keychain: the names its records
/// hold, and any more the keychain lists under its id. Values are never read,
/// so the keychain has nothing to ask. Returns how many were deleted and the
/// names that could not be.
fn remove_secrets(service: &Service, project: &Project, progress: &mut dyn Progress) -> (u64, Vec<String>) {
    let store = service.deps().store(&project.id);
    let mut names: Vec<secrets::SecretName> = Vec::new();
    let mut add = |name: &secrets::SecretName| {
        if !names.iter().any(|n| n == name) {
            names.push(name.clone());
        }
    };
    // Unreadable records still leave the keychain's own list to go by.
    if let Ok(Some(meta)) = hoststate::read_meta(&project.dir) {
        meta.secrets.iter().for_each(|record| add(&record.name));
        meta.assignments.iter().for_each(|assignment| add(assignment.name()));
    }
    if let Ok(Some(listed)) = store.names() {
        listed.iter().for_each(&mut add);
    }
    if names.is_empty() {
        return (0, Vec::new());
    }
    progress.step("removing its secrets from the keychain", None);
    let mut removed = 0u64;
    let mut left = Vec::new();
    for name in &names {
        match store.delete(name) {
            Ok(true) => removed += 1,
            Ok(false) => {}
            Err(_) => left.push(name.as_str().to_owned()),
        }
    }
    if removed > 0 {
        service.record(
            audit::entry(service.deps().now_unix(), audit::Action::SecretsRemoved)
                .project(project.id.as_str())
                .detail(format!("{removed} deleted from the keychain")),
        );
    }
    if !left.is_empty() {
        service.record(
            audit::entry(service.deps().now_unix(), audit::Action::SecretsLeft)
                .project(project.id.as_str())
                .detail(left.join(", ")),
        );
    }
    (removed, left)
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
        ports: record
            .as_ref()
            .map(|r| crate::portset::forwards_of(r).iter().map(|p| p.host).collect())
            .unwrap_or_default(),
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
        usage: if matches!(state, SandboxState::Running) { usage_of(service, project) } else { None },
    })
}

/// A sandbox whose use cannot be read is still a sandbox to report on, so a
/// failure here leaves the figures out rather than failing the status.
fn usage_of(service: &Service, project: &Project) -> Option<Box<proto::SandboxUsage>> {
    let metrics = service.deps().engine().metrics(&project.sandbox).ok()?;
    Some(Box::new(proto::SandboxUsage {
        cpu_millis: metrics.cpu_millis,
        memory_used_bytes: metrics.memory_used_bytes,
        memory_limit_bytes: metrics.memory_limit_bytes,
        memory_host_bytes: metrics.memory_host_bytes,
        disk_used_bytes: metrics.disk_used_bytes,
    }))
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
    #[test]
    fn docker_keeps_container_records_in_memory() {
        let script = super::DOCKER_STATE_IN_MEMORY;
        for folder in [
            "/var/lib/docker/containers",
            "/var/lib/docker/network",
            "/var/lib/docker/containerd/daemon/io.containerd.metadata.v1.bolt",
            "/run/docker",
        ] {
            assert!(script.contains(folder), "{folder} is not kept in memory");
        }
        // Mounted before the tmpfs folders, or dockerd's own bind of its root
        // hides them.
        let bind = script.find("mount --bind /var/lib/docker /var/lib/docker").unwrap();
        assert!(bind < script.find("mount -t tmpfs").unwrap());
        assert!(script.starts_with("\nset -e\n"));
        assert!(script.contains("fstrim /previous-disk"));
        assert!(super::DOCKERD.contains("--feature containerd-snapshotter=false"));
        assert!(super::DOCKERD.starts_with("dockerd "));
    }

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
