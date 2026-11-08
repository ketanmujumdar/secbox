//! Changing a project's ports after its sandbox exists.
//!
//! A person can open or close each port the project publishes, and choose the
//! number it uses on this Mac, for example when two projects both publish
//! 3000. Choosing changes no sandbox. The choice goes into the plan, and the
//! next start applies it, after the person confirms a prompt that names every
//! port it opens. That is the same path the resource limits take
//! ([`crate::resourceset`]), and it is also the only one the engine allows:
//! microsandbox 0.7.13 applies a port change only when a sandbox starts.
//!
//! # What stays fixed
//!
//! - Every forward listens on the host's `127.0.0.1` only (`engine::argv`).
//! - Inside the guest the port keeps the number the project publishes, so the
//!   Compose override ([`crate::ports`]) does not change with the Mac number.
//! - Under a closed network the engine lets a guest port in from the host's
//!   loopback only if the sandbox was created with a rule for it, and it cannot
//!   add one later. A sandbox now gets a rule for every port its plan lists, so
//!   any of them can be opened later. The rule alone reaches nothing: the guest
//!   network is not routed from the host, so only an engine forward reaches it,
//!   and that exists only for an approved port. A sandbox made before this has
//!   rules for the ports it was created with only, and the others cannot be
//!   opened on it.

use engine::PortForward;
use proto::{ApiError, PortChoice, PortChoicesReport, PortPlan, SandboxState, SetPortArgs, Source};

use crate::settings::{Approvals, SandboxRecord};
use crate::{audit, bad, internal, person, plan, run, wrong_state, Project, Service};

/// Lower numbers are kept for the system.
pub const LOWEST_MAC_PORT: u16 = 1024;

/// The forwards a sandbox has, from its record.
pub(crate) fn forwards_of(record: &SandboxRecord) -> Vec<PortForward> {
    let mut forwards: Vec<PortForward> = record
        .ports
        .iter()
        .map(|port| PortForward {
            host: *record.mac_ports.get(port).unwrap_or(port),
            guest: *port,
        })
        .collect();
    forwards.sort_unstable();
    forwards
}

/// The guest ports a sandbox lets in from the host's loopback.
pub(crate) fn let_in(record: &SandboxRecord) -> Vec<u16> {
    record
        .ingress_ports
        .clone()
        .unwrap_or_else(|| record.ports.clone())
}

/// The forwards a start makes: each approved port, at its number on this Mac.
pub(crate) fn wanted(ports: &[PortPlan], approved: &[u16]) -> Vec<PortForward> {
    let mut forwards: Vec<PortForward> = ports
        .iter()
        .filter(|p| approved.contains(&p.host_port))
        .map(|p| PortForward {
            host: p.on_mac(),
            guest: p.host_port,
        })
        .collect();
    forwards.sort_unstable();
    forwards
}

/// What to remove and what to add to go from `now` to `want`.
pub(crate) fn changes(
    now: &[PortForward],
    want: &[PortForward],
) -> (Vec<PortForward>, Vec<PortForward>) {
    let remove = now.iter().filter(|p| !want.contains(p)).copied().collect();
    let add = want.iter().filter(|p| !now.contains(p)).copied().collect();
    (remove, add)
}

/// Why `mac` cannot be used on this Mac for the project's `port`, if it
/// cannot. `others` are the numbers other projects use, with their names.
pub(crate) fn mac_port_problem(
    mac: u16,
    port: u16,
    others: &[(u16, String)],
    in_use: bool,
) -> Option<String> {
    if mac < LOWEST_MAC_PORT && mac != port {
        return Some(format!(
            "port {mac} is below {LOWEST_MAC_PORT}, and those are kept for the system. Choose {LOWEST_MAC_PORT} or above."
        ));
    }
    if let Some((_, name)) = others.iter().find(|(n, _)| *n == mac) {
        return Some(format!(
            "port {mac} is already used by {}. Choose another.",
            proto::text::escape_short(name, 60)
        ));
    }
    if in_use {
        return Some(format!(
            "port {mac} is already in use on this Mac. Choose another."
        ));
    }
    None
}

/// Words for a port in a prompt or message: "3000", or "3001 (web:80)" when
/// it is not the project's own number.
pub(crate) fn describe(forward: &PortForward, ports: &[PortPlan]) -> String {
    match ports.iter().find(|p| p.host_port == forward.guest) {
        Some(p) if forward.host != forward.guest => {
            format!("{} ({}:{})", forward.host, p.service, p.container_port)
        }
        _ => forward.host.to_string(),
    }
}

fn cannot_open_text(port: u16) -> String {
    format!(
        "port {port} cannot be opened on this sandbox, because it was made before ports could change. To open it, run: {} down --destroy, which deletes its data, then start it again.",
        brand::CLI_BIN
    )
}

/// Refuses a start whose ports this sandbox cannot take, before the person is
/// asked anything.
pub(crate) fn check_start(
    project: &Project,
    state: SandboxState,
    want: &[PortForward],
    ports: &[PortPlan],
) -> Result<(), ApiError> {
    if matches!(state, SandboxState::None) {
        return Ok(());
    }
    let Some(record) = SandboxRecord::load(&project.dir).map_err(internal)? else {
        return Ok(());
    };
    let now = forwards_of(&record);
    if now == want {
        return Ok(());
    }
    if matches!(state, SandboxState::Running) {
        let open = if now.is_empty() {
            "no ports".to_owned()
        } else {
            now.iter()
                .map(|p| describe(p, ports))
                .collect::<Vec<_>>()
                .join(", ")
        };
        return Err(wrong_state(format!(
            "it is running with {open} open, and ports change only when it starts. Stop it, then start it again."
        )));
    }
    let allowed = let_in(&record);
    let (_, add) = changes(&now, want);
    if let Some(port) = add.iter().find(|p| !allowed.contains(&p.guest)) {
        return Err(wrong_state(cannot_open_text(port.guest)));
    }
    Ok(())
}

/// Gives a stopped sandbox the forwards of a start a person just approved.
pub(crate) fn apply(
    service: &Service,
    project: &Project,
    want: &[PortForward],
) -> Result<(), ApiError> {
    let Some(mut record) = SandboxRecord::load(&project.dir).map_err(internal)? else {
        return Ok(());
    };
    let now = forwards_of(&record);
    let (remove, add) = changes(&now, want);
    if remove.is_empty() && add.is_empty() {
        return Ok(());
    }
    let allowed = let_in(&record);
    if let Some(port) = add.iter().find(|p| !allowed.contains(&p.guest)) {
        return Err(wrong_state(cannot_open_text(port.guest)));
    }
    service
        .deps()
        .engine()
        .set_ports(&project.sandbox, &remove, &add)
        .map_err(|e| crate::engine_error(e.to_string()))?;

    record.ingress_ports = Some(allowed);
    record.ports = want.iter().map(|p| p.guest).collect();
    record.ports.sort_unstable();
    record.mac_ports = want
        .iter()
        .filter(|p| p.host != p.guest)
        .map(|p| (p.guest, p.host))
        .collect();
    record.save(&project.dir).map_err(internal)?;

    let now_unix = service.deps().now_unix();
    for gone in &remove {
        if let Some(moved) = add.iter().find(|p| p.guest == gone.guest) {
            service.record(
                audit::entry(now_unix, audit::Action::PortMoved)
                    .project(project.id.as_str())
                    .subject(moved.host.to_string())
                    .detail(format!("from {}, for port {}", gone.host, gone.guest)),
            );
        } else {
            service.record(
                audit::entry(now_unix, audit::Action::PortClosed)
                    .project(project.id.as_str())
                    .subject(gone.host.to_string()),
            );
        }
    }
    for new in add
        .iter()
        .filter(|p| !remove.iter().any(|g| g.guest == p.guest))
    {
        let entry = audit::entry(now_unix, audit::Action::PortOpened)
            .project(project.id.as_str())
            .subject(new.host.to_string());
        service.record(if new.host == new.guest {
            entry
        } else {
            entry.detail(format!("for port {}", new.guest))
        });
    }
    Ok(())
}

/// The numbers other projects use on this Mac: what their sandboxes forward,
/// and what they chose to open at their next start.
fn others_using(service: &Service, project: &Project) -> Vec<(u16, String)> {
    let mut used = Vec::new();
    for other in service.known_projects() {
        if other.id == project.id {
            continue;
        }
        let name = person::project_name(&other);
        if let Ok(Some(record)) = SandboxRecord::load(&other.dir) {
            for forward in forwards_of(&record) {
                used.push((forward.host, name.clone()));
            }
        }
        if let Ok(approvals) = Approvals::load(&other.dir) {
            for port in &approvals.ports {
                used.push((*approvals.mac_ports.get(port).unwrap_or(port), name.clone()));
            }
        }
    }
    used
}

fn in_use_on_mac(port: u16) -> bool {
    std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_err()
}

pub fn report(service: &Service, project: &Project) -> Result<PortChoicesReport, ApiError> {
    let plan = match plan::build(service, project, &Source::default()) {
        Ok(plan) => plan,
        Err(error) => {
            return Ok(PortChoicesReport {
                project_id: project.id.as_str().to_owned(),
                ports: Vec::new(),
                problem: Some(error.message),
                restart_needed: false,
            })
        }
    };
    let record = SandboxRecord::load(&project.dir).map_err(internal)?;
    let running = matches!(run::status_of(service, project)?, SandboxState::Running);
    let now = record.as_ref().map(forwards_of).unwrap_or_default();
    let allowed = record.as_ref().map(let_in);

    let ports: Vec<PortChoice> = plan
        .ports
        .iter()
        .map(|p| PortChoice {
            service: p.service.clone(),
            container_port: p.container_port,
            port: p.host_port,
            mac_port: p.on_mac(),
            open: p.chosen.unwrap_or(false),
            open_now: running
                .then(|| now.iter().find(|f| f.guest == p.host_port).map(|f| f.host))
                .flatten(),
            cannot_open: allowed
                .as_ref()
                .filter(|allowed| !allowed.contains(&p.host_port))
                .map(|_| cannot_open_text(p.host_port)),
        })
        .collect();
    let approved: Vec<u16> = ports.iter().filter(|p| p.open).map(|p| p.port).collect();
    let restart_needed = running && record.is_some() && now != wanted(&plan.ports, &approved);
    Ok(PortChoicesReport {
        project_id: project.id.as_str().to_owned(),
        ports,
        problem: None,
        restart_needed,
    })
}

/// Keeps a person's choice for one port. Nothing is asked here: the start that
/// applies it asks, naming the ports it opens.
pub fn set(
    service: &Service,
    project: &Project,
    args: &SetPortArgs,
) -> Result<PortChoicesReport, ApiError> {
    let plan = plan::build(service, project, &Source::default())?;
    let Some(planned) = plan.ports.iter().find(|p| p.host_port == args.port) else {
        let publishes = if plan.ports.is_empty() {
            "nothing".to_owned()
        } else {
            plan.ports
                .iter()
                .map(|p| p.host_port.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        return Err(bad(format!(
            "port {} is not one this project publishes. It publishes {publishes}.",
            args.port
        )));
    };
    let record = SandboxRecord::load(&project.dir).map_err(internal)?;
    if args.open {
        if let Some(record) = &record {
            if !let_in(record).contains(&args.port) {
                return Err(wrong_state(cannot_open_text(args.port)));
            }
        }
    }
    let mac = args.mac_port.unwrap_or_else(|| planned.on_mac());
    if mac == 0 {
        return Err(bad("the port on this Mac must not be 0."));
    }
    if args.open || args.mac_port.is_some() {
        // The sandbox's own forward holds its number already.
        let ours = record
            .as_ref()
            .is_some_and(|r| forwards_of(r).iter().any(|f| f.host == mac));
        let in_use = !ours && in_use_on_mac(mac);
        if let Some(problem) =
            mac_port_problem(mac, args.port, &others_using(service, project), in_use)
        {
            return Err(bad(problem));
        }
    }

    let mut approvals = Approvals::load(&project.dir).map_err(internal)?;
    if !approvals.ports_chosen {
        // Carry on from what is open now, rather than from nothing.
        if let Some(record) = &record {
            approvals.ports = record.ports.clone();
            approvals.mac_ports = record.mac_ports.clone();
        }
    }
    approvals.ports_chosen = true;
    approvals.ports.retain(|p| *p != args.port);
    if args.open {
        approvals.ports.push(args.port);
    }
    if mac == args.port {
        approvals.mac_ports.remove(&args.port);
    } else {
        approvals.mac_ports.insert(args.port, mac);
    }
    approvals.save(&project.dir).map_err(internal)?;
    service.record(
        audit::entry(service.deps().now_unix(), audit::Action::PortChosen)
            .project(project.id.as_str())
            .subject(args.port.to_string())
            .detail(if args.open {
                format!("open on {mac}")
            } else {
                "closed".to_owned()
            }),
    );
    report(service, project)
}

/// The number on this Mac and whether it is chosen, for each planned port.
pub(crate) fn fill_plan(ports: &mut [PortPlan], approvals: &Approvals, has_sandbox: bool) {
    let chosen = approvals.ports_chosen || has_sandbox || !approvals.ports.is_empty();
    for port in ports {
        port.mac_port = *approvals
            .mac_ports
            .get(&port.host_port)
            .unwrap_or(&port.host_port);
        port.chosen = chosen.then(|| approvals.ports.contains(&port.host_port));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn port(service: &str, host_port: u16, container_port: u16) -> PortPlan {
        PortPlan {
            service: service.to_owned(),
            host_port,
            container_port,
            mac_port: 0,
            chosen: None,
            was_all_interfaces: false,
            dynamic: false,
        }
    }

    fn record(ports: Vec<u16>, mac: &[(u16, u16)], ingress: Option<Vec<u16>>) -> SandboxRecord {
        SandboxRecord {
            version: 1,
            created_unix: 0,
            memory_bytes: 0,
            millicpus: 0,
            disk_bytes: 0,
            build_memory_bytes: 0,
            ports,
            plan_digest: String::new(),
            log_level: None,
            reachable_hosts: None,
            baseline: None,
            mac_ports: mac.iter().copied().collect(),
            ingress_ports: ingress,
        }
    }

    #[test]
    fn a_record_forwards_each_port_at_its_mac_number() {
        let r = record(vec![3000, 8080], &[(3000, 3001)], None);
        assert_eq!(
            forwards_of(&r),
            [
                PortForward {
                    host: 3001,
                    guest: 3000
                },
                PortForward::same(8080)
            ]
        );
    }

    #[test]
    fn an_old_sandbox_lets_in_only_the_ports_it_was_made_with() {
        assert_eq!(let_in(&record(vec![3000], &[], None)), [3000]);
        assert_eq!(
            let_in(&record(vec![], &[], Some(vec![3000, 8080]))),
            [3000, 8080]
        );
    }

    #[test]
    fn a_start_forwards_only_approved_ports_at_their_mac_numbers() {
        let mut plan = [port("web", 3000, 80), port("api", 8080, 8080)];
        plan[0].mac_port = 3001;
        assert_eq!(
            wanted(&plan, &[3000]),
            [PortForward {
                host: 3001,
                guest: 3000
            }]
        );
        assert_eq!(wanted(&plan, &[]), []);
        // An older daemon's 0 means the project's own number.
        assert_eq!(wanted(&plan, &[8080]), [PortForward::same(8080)]);
    }

    #[test]
    fn moving_a_port_removes_the_old_forward_and_adds_the_new() {
        let now = [PortForward::same(3000), PortForward::same(8080)];
        let want = [
            PortForward {
                host: 3001,
                guest: 3000,
            },
            PortForward::same(8080),
        ];
        let (remove, add) = changes(&now, &want);
        assert_eq!(remove, [PortForward::same(3000)]);
        assert_eq!(
            add,
            [PortForward {
                host: 3001,
                guest: 3000
            }]
        );
        assert_eq!(changes(&want, &want), (vec![], vec![]));
    }

    #[test]
    fn mac_numbers_below_1024_clashing_or_in_use_are_refused() {
        let others = [(3000, "demo_shop".to_owned())];
        assert!(mac_port_problem(80, 3000, &[], false)
            .unwrap()
            .contains("below 1024"));
        // The project's own low number is not something the person chose.
        assert_eq!(mac_port_problem(80, 80, &[], false), None);
        assert!(mac_port_problem(3000, 3000, &others, false)
            .unwrap()
            .contains("already used by demo_shop"));
        assert!(mac_port_problem(3001, 3000, &[], true)
            .unwrap()
            .contains("in use on this Mac"));
        assert_eq!(mac_port_problem(3001, 3000, &others, false), None);
    }

    #[test]
    fn a_moved_port_is_described_with_its_service() {
        let plan = [port("web", 3000, 80)];
        assert_eq!(
            describe(
                &PortForward {
                    host: 3001,
                    guest: 3000
                },
                &plan
            ),
            "3001 (web:80)"
        );
        assert_eq!(describe(&PortForward::same(3000), &plan), "3000");
    }

    #[test]
    fn the_plan_shows_chosen_ports_only_once_a_person_has_chosen() {
        let mut plan = [port("web", 3000, 80), port("api", 8080, 8080)];
        fill_plan(&mut plan, &Approvals::default(), false);
        assert_eq!(plan[0].chosen, None);
        assert_eq!(plan[0].mac_port, 3000);

        let approvals = Approvals {
            ports: vec![3000],
            mac_ports: BTreeMap::from([(3000, 3001)]),
            ports_chosen: true,
            ..Approvals::default()
        };
        fill_plan(&mut plan, &approvals, true);
        assert_eq!((plan[0].chosen, plan[0].mac_port), (Some(true), 3001));
        assert_eq!((plan[1].chosen, plan[1].mac_port), (Some(false), 8080));
    }
}
