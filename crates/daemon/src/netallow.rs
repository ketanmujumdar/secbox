//! Letting a sandbox reach one more hostname, because a person said so.
//!
//! A sandbox's network rules are fixed when it is created. The engine cannot
//! change them afterwards, and its restore cannot set everything `create` sets:
//! measured on msb 0.7.13, a restored sandbox loses `--net-strict=false`, which
//! breaks HTTPS to an allowed host, and its log level, which stops blocked names
//! being recorded. So the sandbox is replaced:
//!
//! 1. The services stop, then the sandbox, and its root disk is kept as a file.
//! 2. A new sandbox is created as `create` always does, with the new rules and
//!    that disk mounted, and the data is copied across inside the guest.
//! 3. Docker, the secrets and the services start again, if it was running.
//!
//! The kept disk is never read on the host, only copied. It stays until the
//! sandbox is destroyed, because the engine needs it to start the sandbox again.
//!
//! The data is safe at every step. If creating or copying fails, there is no
//! sandbox and the kept disk is still there, and approving again carries on from
//! it. A sandbox is never replaced from an empty new one.
//!
//! What does not carry over: anything installed in the VM itself outside the
//! folders in `COPY_DATA`, and anything in memory.

use std::path::Path;
use std::time::{Duration, Instant};

use engine::{argv, ExecRequest};
use proto::{ApiError, ApprovedHost, ApproveHostReport, BlockHostReport, HostsReport, SandboxState, Source, UpArgs};

use crate::settings::{Approvals, SandboxRecord};
use crate::{audit, bad, engine_error, internal, person, plan, refused, run, secret, Progress, Project, Service};

/// The disk the current sandbox mounts, if it replaced an earlier one.
const KEPT_DISK: &str = "previous-root.raw";
/// The disk just taken from the sandbox being replaced.
const NEW_DISK: &str = "previous-root.new";
const COPY_LIMIT: Duration = Duration::from_secs(25 * 60);

/// Runs in the guest, with the kept disk's folder as `$1`. It writes only inside
/// the guest, and what it copies is the guest's own earlier data.
const COPY_DATA: &str = r#"
set -e
for d in work var/lib/docker root home opt srv usr/local; do
  if [ -d "$1/$d" ]; then
    mkdir -p "/$d"
    cp -a "$1/$d/." "/$d/"
  fi
done
sync
"#;

/// Removes the kept disks, when the sandbox that needs them is destroyed.
pub(crate) fn forget_kept_disk(project: &Project) {
    let _ = std::fs::remove_file(project.dir.join(KEPT_DISK));
    let _ = std::fs::remove_file(project.dir.join(NEW_DISK));
}

/// A hostname as it is stored and compared: lowercase, with no trailing dot, and
/// one the engine's rule syntax cannot read as anything else.
fn clean_host(host: &str) -> Result<String, ApiError> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    argv::validate_host(&host).map_err(|_| {
        bad(format!(
            "{} is not a plain hostname. Name one exact host, with no pattern.",
            proto::text::escape(&host)
        ))
    })?;
    Ok(host)
}

pub fn approve(
    service: &Service,
    project: &Project,
    host: &str,
    progress: &mut dyn Progress,
) -> Result<ApproveHostReport, ApiError> {
    let started = Instant::now();
    let host = clean_host(host)?;
    let state = run::status_of(service, project)?;
    let record = SandboxRecord::load(&project.dir).map_err(internal)?;

    let before = Approvals::load(&project.dir).map_err(internal)?;
    let reachable = record.as_ref().and_then(|r| r.reachable_hosts.as_ref());
    // With no sandbox, the record may be from a try that failed, so it proves nothing.
    let in_place = !matches!(state, SandboxState::None) && reachable.is_some_and(|hosts| hosts.contains(&host));
    if before.run_hosts.contains(&host) && in_place {
        return Ok(report(host, state, false, true, started));
    }

    // Nothing is saved or stopped until the person says yes.
    let reason = format!(
        "let {} reach {host} while it runs, and restart it",
        person::project_name(project)
    );
    service.ask_person(project, person::Act::AllowHost, &reason, progress)?;

    let mut approvals = before.clone();
    if !approvals.run_hosts.contains(&host) {
        approvals.run_hosts.push(host.clone());
    }
    approvals
        .run_hosts_approved_unix
        .insert(host.clone(), service.deps().now_unix());
    approvals.save(&project.dir).map_err(internal)?;
    service.record(
        audit::entry(service.deps().now_unix(), audit::Action::HostApproved)
            .project(project.id.as_str())
            .subject(host.clone()),
    );

    let (sandbox, applied) = replace(service, project, state, record, &before, &host, progress)?;
    if applied {
        crate::blocked::forget_host(project, &host);
    }
    Ok(report(host, sandbox, applied, false, started))
}

/// The hostnames a project's sandbox may reach: from its plan, and approved
/// by a person. Changes nothing.
pub fn list(service: &Service, project: &Project) -> Result<HostsReport, ApiError> {
    let state = run::status_of(service, project)?;
    let record = SandboxRecord::load(&project.dir).map_err(internal)?;
    let approvals = Approvals::load(&project.dir).map_err(internal)?;
    let (plan_hosts, plan_hosts_while_running, plan_note) =
        match plan::build(service, project, &Source::Commit { revision: "HEAD".to_owned() }) {
            Ok(plan) => (plan.network.build_hosts, plan.network.merged_for_whole_life, None),
            Err(error) => (Vec::new(), false, Some(error.message)),
        };
    // With no sandbox, the record may be from a try that failed, so it proves nothing.
    let reachable: Vec<String> = if matches!(state, SandboxState::None) {
        Vec::new()
    } else {
        record.and_then(|r| r.reachable_hosts).unwrap_or_default()
    };
    Ok(hosts_report(state, plan_hosts, plan_hosts_while_running, plan_note, &approvals, &reachable))
}

fn hosts_report(
    sandbox: SandboxState,
    mut plan_hosts: Vec<String>,
    plan_hosts_while_running: bool,
    plan_note: Option<String>,
    approvals: &Approvals,
    reachable: &[String],
) -> HostsReport {
    plan_hosts.sort();
    plan_hosts.dedup();
    let mut approved: Vec<ApprovedHost> = approvals
        .run_hosts
        .iter()
        .map(|host| ApprovedHost {
            host: host.clone(),
            approved_unix: approvals.run_hosts_approved_unix.get(host).copied(),
            in_effect: reachable.contains(host),
        })
        .collect();
    approved.sort_by(|a, b| a.host.cmp(&b.host));
    approved.dedup_by(|a, b| a.host == b.host);
    let mut leaving: Vec<String> = reachable
        .iter()
        .filter(|host| !approvals.run_hosts.contains(host) && !plan_hosts.contains(host))
        .cloned()
        .collect();
    leaving.sort();
    leaving.dedup();
    HostsReport {
        sandbox,
        plan_hosts,
        plan_hosts_while_running,
        plan_note,
        approved,
        leaving,
    }
}

/// Takes back a person's approval of one hostname. A sandbox that exists is
/// replaced without it, as [`approve`] replaces one with it. Nobody is asked:
/// it only narrows what the sandbox can reach.
pub fn block(
    service: &Service,
    project: &Project,
    host: &str,
    progress: &mut dyn Progress,
) -> Result<BlockHostReport, ApiError> {
    let started = Instant::now();
    let host = clean_host(host)?;
    let state = run::status_of(service, project)?;
    let record = SandboxRecord::load(&project.dir).map_err(internal)?;
    let before = Approvals::load(&project.dir).map_err(internal)?;
    let done = |sandbox, applied, was_approved| BlockHostReport {
        host: host.clone(),
        sandbox,
        applied,
        was_approved,
        seconds: started.elapsed().as_secs(),
    };
    if !before.run_hosts.contains(&host) {
        return Ok(done(state, false, false));
    }

    let mut approvals = before.clone();
    approvals.run_hosts.retain(|h| *h != host);
    approvals.run_hosts_approved_unix.remove(&host);
    approvals.save(&project.dir).map_err(internal)?;
    service.record(
        audit::entry(service.deps().now_unix(), audit::Action::HostRevoked)
            .project(project.id.as_str())
            .subject(host.clone()),
    );

    let (sandbox, applied) = replace(service, project, state, record, &before, &host, progress)?;
    Ok(done(sandbox, applied, true))
}

/// Replaces the sandbox so its rules match the approvals just saved, keeping
/// its data. With no sandbox, and no disk left by an earlier try, there is
/// nothing to replace: the next start uses them. If it refuses before anything
/// is stopped, `before` is saved back, so nothing changed.
fn replace(
    service: &Service,
    project: &Project,
    state: SandboxState,
    record: Option<SandboxRecord>,
    before: &Approvals,
    host: &str,
    progress: &mut dyn Progress,
) -> Result<(SandboxState, bool), ApiError> {
    let approvals = Approvals::load(&project.dir).map_err(internal)?;
    let kept = project.dir.join(KEPT_DISK);
    let taken = project.dir.join(NEW_DISK);
    if matches!(state, SandboxState::None) && !kept.exists() && !taken.exists() {
        return Ok((state, false));
    }
    if matches!(state, SandboxState::Unknown) {
        before.save(&project.dir).map_err(internal)?;
        return Err(refused("the sandbox is in a state this version does not understand. Stop it and try again."));
    }

    // Everything that can refuse comes before anything is stopped.
    progress.step("planning", None);
    let source = Source::Commit { revision: "HEAD".to_owned() };
    let plan = match plan::build(service, project, &source) {
        Ok(plan) if plan.blockers.is_empty() => plan,
        Ok(plan) => {
            before.save(&project.dir).map_err(internal)?;
            return Err(refused(format!("it cannot be replaced yet: {}", plan.blockers.join(" / "))));
        }
        Err(error) => {
            before.save(&project.dir).map_err(internal)?;
            return Err(error);
        }
    };
    let ports = record.as_ref().map_or_else(|| approvals.ports.clone(), |r| r.ports.clone());
    let baseline = record.as_ref().and_then(|r| r.baseline.clone());
    // With no sandbox this carries on from an earlier try, and starts it again.
    let was_stopped = matches!(state, SandboxState::Stopped);

    if !matches!(state, SandboxState::None) {
        retire(service, project, state, &taken, progress)?;
    }
    if taken.exists() {
        std::fs::rename(&taken, &kept)
            .map_err(|e| internal(format!("cannot keep the sandbox's disk: {}", e.kind())))?;
    }
    if kept.exists() {
        // It is the earlier sandbox's whole disk: only this user may read it.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&kept, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| internal(format!("cannot make the kept disk private: {}", e.kind())))?;
    }

    progress.step("new sandbox", Some(host));
    let args = UpArgs {
        project: project.root.to_string_lossy().into_owned(),
        source,
        plan_digest: plan.digest.clone(),
        approve_ports: ports.clone(),
    };
    run::create(service, project, &plan, &ports, &args, Some(kept.clone()))?;
    if let Err(error) = copy_data(service, project, progress) {
        // An empty new sandbox must not be mistaken for the real one by a retry,
        // which would keep its empty disk over the data.
        let _ = service.deps().engine().destroy(&project.sandbox);
        // `create` wrote a new record, and the next try needs what the old one knew.
        if let Some(record) = &record {
            record.save(&project.dir).map_err(internal)?;
        }
        return Err(error);
    }
    keep_baseline(project, baseline)?;

    if was_stopped {
        service
            .deps()
            .engine()
            .stop(&project.sandbox)
            .map_err(|e| engine_error(e.to_string()))?;
        return Ok((SandboxState::Stopped, true));
    }
    progress.step("docker", None);
    run::start_docker(service, project)?;
    progress.step("secrets", None);
    let delivered = secret::deliver(service, project)?;
    progress.step("services", None);
    run::compose_up(service, project, &plan, &delivered)?;
    Ok((SandboxState::Running, true))
}

/// Stops the sandbox being replaced and takes its disk, then removes it. The disk
/// is taken first, so a failure leaves the sandbox as it was, only stopped.
fn retire(
    service: &Service,
    project: &Project,
    state: SandboxState,
    taken: &Path,
    progress: &mut dyn Progress,
) -> Result<(), ApiError> {
    if matches!(state, SandboxState::Running) {
        run::down(service, project, false, progress)?;
    }
    // The log goes with the sandbox.
    crate::blocked::collect_now(service, project);
    let engine = service.deps().engine();
    progress.step("keeping its data", None);
    engine
        .keep_root_disk(&project.sandbox, taken)
        .map_err(|e| engine_error(e.to_string()))?;
    engine.destroy(&project.sandbox).map_err(|e| engine_error(e.to_string()))
}

fn copy_data(service: &Service, project: &Project, progress: &mut dyn Progress) -> Result<(), ApiError> {
    progress.step("copying its data", None);
    let mut request = ExecRequest::new(["sh", "-c", COPY_DATA, "copy-data", argv::PREVIOUS_DISK]);
    request.timeout = Some(COPY_LIMIT);
    let out = service
        .deps()
        .engine()
        .exec(&project.sandbox, &request)
        .map_err(|e| engine_error(e.to_string()))?;
    if out.code != 0 {
        return Err(internal(format!(
            "could not copy the data across, so it is still in the kept disk. Approving again carries on from there. {}",
            proto::text::escape(out.stderr.trim())
        )));
    }
    Ok(())
}

/// `create` starts a new record. The code is the same as before, so what `diff`
/// compares with is too.
fn keep_baseline(project: &Project, baseline: Option<String>) -> Result<(), ApiError> {
    if let Some(mut record) = SandboxRecord::load(&project.dir).map_err(internal)? {
        record.baseline = baseline;
        record.save(&project.dir).map_err(internal)?;
    }
    Ok(())
}

fn report(host: String, sandbox: SandboxState, applied: bool, already_allowed: bool, started: Instant) -> ApproveHostReport {
    ApproveHostReport {
        host,
        sandbox,
        applied,
        already_allowed,
        seconds: started.elapsed().as_secs(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approvals(hosts: &[&str]) -> Approvals {
        Approvals {
            run_hosts: hosts.iter().map(|h| (*h).to_owned()).collect(),
            run_hosts_approved_unix: [("example.com".to_owned(), 42)].into_iter().collect(),
            ..Approvals::default()
        }
    }

    #[test]
    fn hosts_are_split_into_the_plan_approved_and_leaving() {
        let reachable = ["registry-1.docker.io", "example.com", "old.example.org"].map(str::to_owned);
        let report = hosts_report(
            SandboxState::Running,
            vec!["registry-1.docker.io".to_owned(), "auth.docker.io".to_owned()],
            true,
            None,
            &approvals(&["example.com", "later.example.net"]),
            &reachable,
        );
        assert_eq!(report.plan_hosts, vec!["auth.docker.io", "registry-1.docker.io"]);
        assert_eq!(
            report.approved,
            vec![
                ApprovedHost { host: "example.com".into(), approved_unix: Some(42), in_effect: true },
                ApprovedHost { host: "later.example.net".into(), approved_unix: None, in_effect: false },
            ]
        );
        // Blocked again, but the sandbox has not been replaced yet.
        assert_eq!(report.leaving, vec!["old.example.org"]);
    }

    #[test]
    fn with_no_sandbox_nothing_is_in_effect_or_leaving() {
        let report = hosts_report(SandboxState::None, Vec::new(), false, Some("x".into()), &approvals(&["example.com"]), &[]);
        assert!(!report.approved[0].in_effect);
        assert!(report.leaving.is_empty());
        assert_eq!(report.plan_note.as_deref(), Some("x"));
    }
}
