//! The full-screen interface.
//!
//! It is one more client of the daemon, with no powers the command line
//! lacks: every action here becomes the same request, and the daemon applies
//! the same rules. It needs no web view and no front-end toolchain, which is
//! the reason to try it before a desktop app.
//!
//! Starting a project still needs a person to agree, so the start key opens a
//! box showing what would happen and waits. Nothing is forwarded by a
//! keystroke that did not see what it was agreeing to.

mod screen;
mod style;
mod term;
mod view;

use std::io::{BufRead, IsTerminal};

use proto::{DownArgs, PlanArgs, PlanReport, ProjectArgs, Reply, Request, SandboxState, Source};

use crate::{exit, tell, Deps};
use style::Style;
use view::{Dialog, Key, Line, Model, PaneState, Row, RowState};

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
        let key = view::key(&bytes);

        if pending.is_some() {
            match key {
                Key::Yes => {
                    let waiting = pending.take().expect("checked just above");
                    model.dialog = None;
                    start(deps, &mut model, waiting);
                }
                Key::No | Key::Quit => {
                    pending = None;
                    model.dialog = None;
                    model.message = Some("Nothing was started.".to_owned());
                }
                _ => {}
            }
            continue;
        }

        match key {
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
            Key::Help => {
                model.message = Some(
                    "Arrows move. 2 or u starts, 3 or s stops, x destroys, 5 refreshes, 6 or tab changes panel, 10 or q quits."
                        .to_owned(),
                );
            }
            Key::Yes | Key::No | Key::Other => {}
        }
    }
    drop(raw);
    exit::OK
}

fn draw(deps: &mut Deps, model: &Model) {
    let (width, height) = term::size(deps.input, deps.out);
    let rows = view::render(model, width as usize, height as usize);
    let mut out = String::from("\x1b[H");
    let last = rows.len().saturating_sub(1);
    for (index, row) in rows.iter().enumerate() {
        for span in row {
            if deps.color {
                out.push_str("\x1b[");
                out.push_str(span.style.codes());
                out.push('m');
                out.push_str(&span.text);
            } else {
                out.push_str(&span.text);
            }
        }
        if deps.color {
            out.push_str("\x1b[0m");
        }
        // No newline after the last row: on many terminals writing to the
        // final cell scrolls the screen, which would shift everything up.
        if index != last {
            out.push_str("\r\n");
        }
    }
    let _ = deps.out.write_all(out.as_bytes());
    let _ = deps.out.flush();
}

/// Reads one keystroke. An escape sequence usually arrives in one read, so
/// the whole buffer is handed to the key reader rather than one byte.
///
/// This reads the input handle it was given, never the process's own standard
/// input. The caller holds a lock on that, and taking it again deadlocks the
/// interface on the first keystroke, with no way left to quit.
fn read_key(input: &mut dyn BufRead) -> Option<Vec<u8>> {
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
    model.projects = crate::state::known_projects(&paths)
        .iter()
        .map(|(id, record)| Row {
            name: id.as_str().to_owned(),
            path: record.path.clone(),
            state: RowState::Unknown,
        })
        .collect();
    model.selected = model.selected.min(model.projects.len().saturating_sub(1));
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
    match deps.daemon.call(
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
    model.detail.push(Line::dim(row.path.clone()));
    let Some(report) = status_of(deps, &row.path) else {
        model
            .detail
            .push(Line::styled("the daemon did not answer", Style::WindowAlert));
        return;
    };
    if let Some(resources) = &report.resources {
        model.detail.push(Line::plain(format!(
            "{} memory, {} CPUs",
            crate::ui::human_bytes(resources.memory_bytes),
            f64::from(resources.millicpus) / 1000.0
        )));
    }
    if !report.ports.is_empty() {
        model.detail.push(Line::styled(
            format!(
                "forwarded  {}",
                report
                    .ports
                    .iter()
                    .map(|p| format!("127.0.0.1:{p}"))
                    .collect::<Vec<_>>()
                    .join("  ")
            ),
            Style::WindowGood,
        ));
    }
    for service in report.services.iter().take(10) {
        model.detail.push(Line::plain(format!(
            "  {:<12}{}{}",
            screen::cut(&service.name, 12),
            service.state,
            service
                .health
                .as_deref()
                .map(|h| format!(" ({h})"))
                .unwrap_or_default()
        )));
    }
    if !report.services.is_empty() {
        model.detail.push(Line::styled(
            "these came from inside the sandbox, so they can be wrong",
            Style::WindowWarn,
        ));
    }
}

/// Builds a plan and shows it in a box, so that Yes agrees to something seen.
fn propose_start(deps: &mut Deps, model: &mut Model) -> Option<Pending> {
    let row = model.selected_row().cloned()?;
    let plan = match deps.daemon.call(
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
    if !plan.blockers.is_empty() {
        model.detail = plan
            .blockers
            .iter()
            .map(|b| Line::styled(b.clone(), Style::WindowAlert))
            .collect();
        model.message = Some("This cannot be started. See the panel.".to_owned());
        return None;
    }
    let ports: Vec<u16> = plan.ports.iter().map(|p| p.host_port).collect();
    model.dialog = Some(Dialog {
        title: format!("Start {}", screen::cut(&row.name, 30)),
        lines: dialog_lines(&plan, &ports),
    });
    Some(Pending {
        project: row.path,
        digest: plan.digest,
        ports,
    })
}

fn dialog_lines(plan: &PlanReport, ports: &[u16]) -> Vec<String> {
    let mut lines = vec![
        format!(
            "{} services from commit {}",
            plan.services.len(),
            plan.snapshot
                .revision
                .as_deref()
                .map_or("the working tree", |r| r.get(..12).unwrap_or(r))
        ),
        String::new(),
    ];
    if ports.is_empty() {
        lines.push("No ports would be forwarded.".to_owned());
    } else {
        lines.push(format!(
            "Forward to 127.0.0.1:  {}",
            ports
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join("  ")
        ));
        lines.push("Anything on this machine could then reach them.".to_owned());
    }
    lines.push(String::new());
    lines.push(format!(
        "{} hosts allowed for the build, {} while it runs.",
        plan.network.build_hosts.len(),
        plan.network.run_hosts.len()
    ));
    if plan.network.merged_for_whole_life {
        lines.push("Build hosts stay reachable after the build.".to_owned());
    }
    lines
}

fn start(deps: &mut Deps, model: &mut Model, pending: Pending) {
    model.busy = Some("Starting. A first build can take minutes.".to_owned());
    draw(deps, model);
    let result = deps.daemon.call(
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
        Ok(Reply::Up(report)) => format!("Started in {}s.", report.seconds),
        Ok(_) => "The daemon answered something else.".to_owned(),
        Err(error) => error.message(),
    });
    load_projects(deps, model);
}

fn stop(deps: &mut Deps, model: &mut Model, destroy: bool) {
    let Some(row) = model.selected_row().cloned() else {
        return;
    };
    model.busy = Some(if destroy { "Destroying." } else { "Stopping." }.to_owned());
    draw(deps, model);
    let result = deps.daemon.call(
        Request::Down(DownArgs {
            project: row.path,
            destroy,
        }),
        &mut |_, _| {},
    );
    model.busy = None;
    model.message = Some(match result {
        Ok(Reply::Down(report)) if report.destroyed => {
            "Destroyed. Rotate every secret it was given.".to_owned()
        }
        Ok(Reply::Down(_)) => "Stopped. Its data is kept for the next start.".to_owned(),
        Ok(_) => "The daemon answered something else.".to_owned(),
        Err(error) => error.message(),
    });
    load_projects(deps, model);
}
