//! The limits a person chooses for a project.
//!
//! Choosing changes no sandbox. The choice goes into the plan, a person sees it
//! there before a start, and only then does a stopped sandbox change: the same
//! path every other change to a sandbox takes. That is also why saving does not
//! ask the person to confirm, since the start that applies it does.

use proto::{ApiError, ResourceChoice, ResourceChoicesReport, SandboxState};

use crate::settings::{Approvals, SandboxRecord, UserSettings};
use crate::{audit, bad, internal, plan, run, wrong_state, Project, Service};

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;
/// Too small for the guest to boot Docker in, whatever the project needs.
const MIN_MEMORY_BYTES: u64 = 512 * MIB;

pub fn report(service: &Service, project: &Project) -> Result<ResourceChoicesReport, ApiError> {
    let chosen = Approvals::load(&project.dir).map_err(internal)?.resources;
    let settings = UserSettings::load(service.state_dir()).map_err(internal)?;
    let current = SandboxRecord::load(&project.dir).map_err(internal)?.map(|r| proto::ResourcePlan {
        memory_bytes: r.memory_bytes,
        millicpus: r.millicpus,
        disk_bytes: r.disk_bytes,
        build_memory_bytes: r.build_memory_bytes,
        clamped: false,
    });
    let (next, problem) = match plan::resolve_with(service, project, &settings, &chosen)? {
        Ok(resolved) => (Some(plan::resource_plan(&resolved, project)?), None),
        Err(error) => (None, Some(proto::text::escape(&error.to_string()))),
    };
    let same_as_now = |a: &proto::ResourcePlan, b: &proto::ResourcePlan| {
        (a.memory_bytes, a.build_memory_bytes, a.millicpus, a.disk_bytes)
            == (b.memory_bytes, b.build_memory_bytes, b.millicpus, b.disk_bytes)
    };
    let restart_needed = matches!(run::status_of(service, project)?, SandboxState::Running)
        && matches!((&current, &next), (Some(a), Some(b)) if !same_as_now(a, b));
    let host = plan::host_now(service, project, &settings);
    Ok(ResourceChoicesReport {
        project_id: project.id.as_str().to_owned(),
        chosen,
        current,
        next,
        problem,
        restart_needed,
        max_memory_bytes: host.available_memory.saturating_sub(host.memory_reserve),
        max_millicpus: host.available_millicpus.saturating_sub(host.cpu_reserve_millis),
        max_disk_bytes: host.available_disk.saturating_sub(host.disk_reserve),
    })
}

/// Keeps a choice this machine can honour now. One it cannot is refused here,
/// where the person can change it, rather than at the next start.
pub fn set(service: &Service, project: &Project, choice: ResourceChoice) -> Result<ResourceChoicesReport, ApiError> {
    if choice.memory_bytes.is_some_and(|m| m < MIN_MEMORY_BYTES) {
        return Err(bad("memory must be at least 512 MB."));
    }
    if choice.millicpus.is_some_and(|c| c == 0 || c % 1000 != 0) {
        return Err(bad("CPUs must be a whole number above zero."));
    }
    if choice.disk_bytes.is_some_and(|d| d == 0 || d % GIB != 0) {
        return Err(bad("disk must be a whole number of GB above zero."));
    }
    let record = SandboxRecord::load(&project.dir).map_err(internal)?;
    if let (Some(disk), Some(record)) = (choice.disk_bytes, &record) {
        if disk < record.disk_bytes {
            return Err(wrong_state(format!(
                "this sandbox's disk is already {} GB, and a disk can only grow. To make it smaller, destroy the sandbox, which deletes its data, then start it again.",
                record.disk_bytes / GIB
            )));
        }
    }
    let settings = UserSettings::load(service.state_dir()).map_err(internal)?;
    if let Err(error) = plan::resolve_with(service, project, &settings, &choice)? {
        return Err(bad(proto::text::escape(&error.to_string())));
    }

    let mut approvals = Approvals::load(&project.dir).map_err(internal)?;
    approvals.resources = choice;
    approvals.save(&project.dir).map_err(internal)?;
    service.record(
        audit::entry(service.deps().now_unix(), audit::Action::ResourcesChosen).project(project.id.as_str()),
    );
    report(service, project)
}
