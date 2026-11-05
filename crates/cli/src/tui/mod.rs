//! The full-screen interface.
//!
//! It is one more client of the daemon, with no powers the command line
//! lacks: every action here becomes the same request, and the daemon applies
//! the same rules. It needs no web view and no front-end toolchain, which is
//! the reason to try it before a desktop app.
//!
//! Starting a project still needs a person to agree to the ports, so `u`
//! shows the plan and waits for `y`. Nothing is forwarded by a keystroke that
//! did not see what it was agreeing to.

mod term;
mod view;

use std::io::IsTerminal;

use proto::{DownArgs, PlanArgs, PlanReport, ProjectArgs, Reply, Request, SandboxState, Source};

use crate::ui::{paint, Tone};
use crate::{exit, tell, Deps};
use view::{Key, Line, Model, PaneState, Row, RowState};

/// What a keystroke is waiting for a person to confirm.
struct Pending {
    project: String,
    digest: String,
    ports: Vec<u16>,
}

pub fn run(deps: &mut Deps) -> i32 {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        tell!(
            deps,
            "{}: this needs a terminal. Run {} --help for the commands.",
            brand::CLI_BIN,
            brand::CLI_BIN
        );
        return exit::USAGE;
    }
    let raw = match term::RawMode::enter() {
        Ok(raw) => raw,
        Err(reason) => {
            tell!(deps, "{}: {reason}", brand::CLI_BIN);
            return exit::FAILED;
        }
    };

    let mut model = Model::default();
    let mut pending: Option<Pending> = None;
    load_projects(deps, &mut model);

    loop {
        draw(deps, &model);
        let Some(bytes) = read_key(deps.input) else { break };
        if let Some(waiting) = pending.take() {
            match bytes.as_slice() {
                b"y" | b"Y" => start(deps, &mut model, waiting),
                _ => model.message = Some("Nothing was started.".to_owned()),
            }
            continue;
        }
        match view::key(&bytes) {
            Key::Quit => break,
            Key::Up => {
                model.move_by(-1);
                show_selected(deps, &mut model);
            }
            Key::Down => {
                model.move_by(1);
                show_selected(deps, &mut model);
            }
            Key::Tab => {
                model.pane = PaneState(model.pane.0.next());
                model.message = None;
            }
            Key::Refresh => load_projects(deps, &mut model),
            Key::Start => pending = propose_start(deps, &mut model),
            Key::Stop => stop(deps, &mut model, false),
            Key::Destroy => stop(deps, &mut model, true),
            Key::Other => {}
        }
    }
    drop(raw);
    exit::OK
}

fn draw(deps: &mut Deps, model: &Model) {
    let (width, height) = term::size(deps.input, deps.out);
    let lines = view::render(model, width as usize, height as usize);
    let mut screen = String::from("\x1b[H\x1b[2J");
    for line in lines {
        screen.push_str(&paint(deps.color, line.tone, &line.text));
        screen.push_str("\r\n");
    }
    let _ = deps.out.write_all(screen.as_bytes());
    let _ = deps.out.flush();
}

/// Reads one keystroke. An escape sequence usually arrives in one read, so
/// the whole buffer is handed to the key reader rather than one byte.
///
/// This reads the input handle it was given, never the process's own standard
/// input. The caller holds a lock on that, and taking it again deadlocks the
/// interface on the first keystroke, with no way left to quit.
fn read_key(input: &mut dyn std::io::BufRead) -> Option<Vec<u8>> {
    let mut buffer = [0u8; 8];
    match input.read(&mut buffer) {
        Ok(0) | Err(_) => None,
        Ok(read) => Some(buffer[..read].to_vec()),
    }
}

fn load_projects(deps: &mut Deps, model: &mut Model) {
    model.message = None;
    let Ok(paths) = crate::state::StatePaths::locate(deps.env) else {
        model.message = Some("cannot find the state folder".to_owned());
        return;
    };
    let known = crate::state::known_projects(&paths);
    model.projects = known
        .iter()
        .map(|(id, record)| Row {
            name: id.as_str().to_owned(),
            path: record.path.clone(),
            state: RowState::Unknown,
        })
        .collect();
    model.selected = model.selected.min(model.projects.len().saturating_sub(1));
    // Each row's state comes from the daemon, one request each. A project
    // whose folder has gone is left as unknown rather than dropped, because
    // the person may want to see that it is gone.
    for index in 0..model.projects.len() {
        let path = model.projects[index].path.clone();
        if let Some(report) = status_of(deps, &path) {
            model.projects[index].state = match report.sandbox {
                SandboxState::Running => RowState::Running,
                SandboxState::Stopped => RowState::Stopped,
                SandboxState::None => RowState::None,
                SandboxState::Unknown => RowState::Unknown,
            };
        }
    }
    show_selected(deps, model);
}

fn status_of(deps: &mut Deps, path: &str) -> Option<proto::StatusReport> {
    let client = deps.daemon;
    match client.call(
        Request::Status(ProjectArgs {
            project: path.to_owned(),
        }),
        &mut |_, _| {},
    ) {
        Ok(Reply::Status(report)) => Some(report),
        _ => None,
    }
}

fn show_selected(deps: &mut Deps, model: &mut Model) {
    model.detail.clear();
    let Some(row) = model.selected_row().cloned() else {
        return;
    };
    model.detail.push(Line {
        text: row.path.clone(),
        tone: Tone::Dim,
    });
    let Some(report) = status_of(deps, &row.path) else {
        model.detail.push(Line {
            text: "the daemon did not answer for this project".to_owned(),
            tone: Tone::Bad,
        });
        return;
    };
    if let Some(resources) = &report.resources {
        model.detail.push(Line {
            text: format!(
                "{} memory, {} CPUs",
                crate::ui::human_bytes(resources.memory_bytes),
                f64::from(resources.millicpus) / 1000.0
            ),
            tone: Tone::Dim,
        });
    }
    if !report.ports.is_empty() {
        model.detail.push(Line {
            text: format!(
                "forwarded: {}",
                report
                    .ports
                    .iter()
                    .map(|p| format!("127.0.0.1:{p}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            tone: Tone::Good,
        });
    }
    for service in report.services.iter().take(12) {
        model.detail.push(Line {
            text: format!(
                "{} {}{}",
                service.name,
                service.state,
                service
                    .health
                    .as_deref()
                    .map(|h| format!(" ({h})"))
                    .unwrap_or_default()
            ),
            tone: Tone::Dim,
        });
    }
    if !report.services.is_empty() {
        model.detail.push(Line {
            text: "these came from inside the sandbox, so they can be wrong".to_owned(),
            tone: Tone::Warn,
        });
    }
}

/// Builds a plan and shows it, so that `y` is agreeing to something seen.
fn propose_start(deps: &mut Deps, model: &mut Model) -> Option<Pending> {
    let row = model.selected_row().cloned()?;
    let client = deps.daemon;
    let plan = match client.call(
        Request::Plan(PlanArgs {
            project: row.path.clone(),
            source: Source::default(),
        }),
        &mut |_, _| {},
    ) {
        Ok(Reply::Plan(plan)) => *plan,
        Ok(_) => return None,
        Err(error) => {
            model.message = Some(error.message());
            return None;
        }
    };
    model.detail = plan_lines(&plan);
    if !plan.blockers.is_empty() {
        model.message = Some("This cannot be started. See the panel.".to_owned());
        return None;
    }
    let ports: Vec<u16> = plan.ports.iter().map(|p| p.host_port).collect();
    model.message = Some(if ports.is_empty() {
        "Start it? y to agree, anything else to cancel.".to_owned()
    } else {
        format!(
            "Start it and forward {} to 127.0.0.1? y to agree, anything else to cancel.",
            ports
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    });
    Some(Pending {
        project: row.path,
        digest: plan.digest,
        ports,
    })
}

fn plan_lines(plan: &PlanReport) -> Vec<Line> {
    let mut lines = vec![Line {
        text: format!("{} services, {} ports", plan.services.len(), plan.ports.len()),
        tone: Tone::Strong,
    }];
    for blocker in &plan.blockers {
        lines.push(Line {
            text: blocker.clone(),
            tone: Tone::Bad,
        });
    }
    for port in &plan.ports {
        lines.push(Line {
            text: format!("{} -> {} in {}", port.host_port, port.container_port, port.service),
            tone: Tone::Dim,
        });
    }
    lines.push(Line {
        text: format!(
            "{} hosts for the build, {} while it runs; build hosts stay reachable after it",
            plan.network.build_hosts.len(),
            plan.network.run_hosts.len()
        ),
        tone: Tone::Warn,
    });
    lines
}

fn start(deps: &mut Deps, model: &mut Model, pending: Pending) {
    model.busy = Some("starting, which can take minutes".to_owned());
    draw(deps, model);
    let client = deps.daemon;
    let result = client.call(
        Request::Up(proto::UpArgs {
            project: pending.project,
            source: Source::default(),
            plan_digest: pending.digest,
            approve_ports: pending.ports,
        }),
        &mut |_, _| {},
    );
    model.busy = None;
    model.message = Some(match result {
        Ok(Reply::Up(report)) => format!("started in {}s", report.seconds),
        Ok(_) => "the daemon answered something else".to_owned(),
        Err(error) => error.message(),
    });
    load_projects(deps, model);
}

fn stop(deps: &mut Deps, model: &mut Model, destroy: bool) {
    let Some(row) = model.selected_row().cloned() else {
        return;
    };
    model.busy = Some(if destroy { "destroying" } else { "stopping" }.to_owned());
    draw(deps, model);
    let client = deps.daemon;
    let result = client.call(
        Request::Down(DownArgs {
            project: row.path,
            destroy,
        }),
        &mut |_, _| {},
    );
    model.busy = None;
    model.message = Some(match result {
        Ok(Reply::Down(report)) if report.destroyed => {
            "destroyed. Rotate every secret it was given.".to_owned()
        }
        Ok(Reply::Down(_)) => "stopped".to_owned(),
        Ok(_) => "the daemon answered something else".to_owned(),
        Err(error) => error.message(),
    });
    load_projects(deps, model);
}

#[cfg(test)]
mod tests {
    /// The panel contents and the key map are tested in `view`, which is the
    /// part with no terminal in it. What is left here is the loop, and that
    /// needs a real terminal, so it is exercised by hand.
    #[test]
    fn the_pane_labels_do_not_promise_anything_that_works() {
        use super::view::Pane;
        for pane in Pane::ALL {
            assert!(!pane.label().is_empty());
        }
    }
}
