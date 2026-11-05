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

mod logs;
mod overview;
mod screen;
mod secrets;
mod style;
mod term;
mod traffic;
mod view;

use std::io::IsTerminal;
use std::time::{Duration, Instant};

use proto::{DownArgs, PlanArgs, PlanReport, Reply, Request, SandboxState, Source};

use crate::{debug, exit, tell, Deps};
use style::Style;
pub(crate) use term::SavedSettings;
use view::{Dialog, Key, Line, Model, Pane, PaneState, Row, RowState};

/// What a keystroke is waiting for a person to confirm.
struct Pending {
    name: String,
    project: String,
    digest: String,
    ports: Vec<u16>,
}

/// How many lines the page keys move through the logs.
const LOG_PAGE: isize = 20;

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
    let mut confirming: Option<bool> = None;
    // A Git remote being typed, for Import.
    let mut importing: Option<String> = None;
    // A box that only reports, closed by any key.
    let mut notice = false;
    // The log being followed, if the Logs panel is showing one.
    let mut follow: Option<logs::Follow> = None;
    let mut keyboard = term::Keyboard::default();
    load_projects(deps, &mut model);

    let mut queued: Vec<u8> = Vec::new();
    let mut dirty = true;
    loop {
        if let Some(live) = &mut follow {
            dirty |= live.take(&mut model.logs);
            if live.over() {
                follow = None;
            }
        }
        // Drawing asks the terminal its size, so it is done only when
        // something changed, not on every tick.
        if dirty {
            draw(deps, &model);
            dirty = false;
        }
        // A question is answered by a key pressed after it was seen, never by
        // one already on its way, such as from a paste.
        if confirming.is_some() || pending.is_some() || notice {
            queued.clear();
        }
        if queued.is_empty() {
            match keyboard.read(deps.input) {
                term::Input::Bytes(bytes) => queued = bytes,
                term::Input::Idle => continue,
                term::Input::Closed => break,
            }
        }
        dirty = true;
        // Typing a remote takes the bytes themselves, pastes included.
        if let Some(text) = importing.as_mut() {
            let bytes = std::mem::take(&mut queued);
            match edit_line(text, &bytes) {
                LineEdit::Typing => model.dialog = Some(import_dialog(text)),
                LineEdit::Cancel => {
                    importing = None;
                    model.dialog = None;
                    model.message = Some("Nothing was imported.".to_owned());
                }
                LineEdit::Submit => {
                    let url = importing.take().unwrap_or_default();
                    model.dialog = None;
                    pending = import(deps, &mut model, url.trim());
                }
            }
            continue;
        }
        let taken = view::first_key_len(&queued);
        let key = view::key(&queued[..taken]);
        queued.drain(..taken);

        if notice {
            notice = false;
            model.dialog = None;
            continue;
        }

        if let Some(destroy) = confirming {
            match key {
                Key::Yes => {
                    confirming = None;
                    model.dialog = None;
                    stop(deps, &mut model, destroy);
                    refresh_panel_if_showing(deps, &mut model, &mut follow);
                }
                Key::No | Key::Quit => {
                    confirming = None;
                    model.dialog = None;
                    model.message = Some("Nothing was changed.".to_owned());
                }
                _ => {}
            }
            continue;
        }

        if pending.is_some() {
            match key {
                Key::Yes => {
                    let waiting = pending.take().expect("checked just above");
                    model.dialog = None;
                    start(deps, &mut model, waiting);
                    refresh_panel_if_showing(deps, &mut model, &mut follow);
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
            Key::Up if model.pane.0 == Pane::Logs => model.logs.scroll_by(1),
            Key::Down if model.pane.0 == Pane::Logs => model.logs.scroll_by(-1),
            Key::PageUp if model.pane.0 == Pane::Logs => model.logs.scroll_by(LOG_PAGE),
            Key::PageDown if model.pane.0 == Pane::Logs => model.logs.scroll_by(-LOG_PAGE),
            Key::Follow if model.pane.0 == Pane::Logs => model.logs.jump_to_newest(),
            Key::Up if model.pane.0 == Pane::Traffic => model.traffic.scroll_by(1),
            Key::Down if model.pane.0 == Pane::Traffic => model.traffic.scroll_by(-1),
            Key::PageUp if model.pane.0 == Pane::Traffic => model.traffic.scroll_by(LOG_PAGE),
            Key::PageDown if model.pane.0 == Pane::Traffic => model.traffic.scroll_by(-LOG_PAGE),
            Key::Up => {
                model.move_by(-1);
                show_selected(deps, &mut model);
                refresh_panel_if_showing(deps, &mut model, &mut follow);
            }
            Key::Down => {
                model.move_by(1);
                show_selected(deps, &mut model);
                refresh_panel_if_showing(deps, &mut model, &mut follow);
            }
            Key::Tab => {
                model.pane = PaneState(model.pane.0.next());
                model.message = None;
                refresh_panel_if_showing(deps, &mut model, &mut follow);
            }
            Key::Refresh => {
                load_projects(deps, &mut model);
                refresh_panel_if_showing(deps, &mut model, &mut follow);
            }
            Key::PreviousService | Key::NextService if model.pane.0 == Pane::Logs => {
                model.logs.choose(if key == Key::NextService { 1 } else { -1 });
                refresh_panel_if_showing(deps, &mut model, &mut follow);
            }
            // What acts on a sandbox works only where the project list is,
            // so a key pressed while reading something else cannot stop one.
            Key::Start if model.pane.0 == Pane::Projects => {
                pending = propose_start(deps, &mut model);
            }
            Key::Shell if matches!(model.pane.0, Pane::Projects | Pane::Logs) => {
                open_shell(deps, &mut model, &raw);
                // Whatever was typed for the shell was the shell's.
                queued.clear();
            }
            Key::Import if model.pane.0 == Pane::Projects => {
                let text = String::new();
                model.dialog = Some(import_dialog(&text));
                importing = Some(text);
            }
            Key::Pull | Key::Push if model.pane.0 == Pane::Projects => {
                if git_on_selected(deps, &mut model, key == Key::Push) {
                    notice = true;
                }
                queued.clear();
                refresh_panel_if_showing(deps, &mut model, &mut follow);
            }
            Key::Open(pane) => {
                model.pane = PaneState(pane);
                model.message = None;
                refresh_panel_if_showing(deps, &mut model, &mut follow);
            }
            Key::Stop if model.pane.0 == Pane::Projects => {
                if let Some(row) = model.selected_row().cloned() {
                    model.dialog = Some(Dialog {
                        title: format!("Stop {}", screen::cut(&row.name, 30)),
                        lines: vec![
                            "Its services stop and their ports close.".to_owned(),
                            "Its data is kept for the next start.".to_owned(),
                        ],
                        footer: None,
                    });
                    confirming = Some(false);
                }
            }
            Key::Destroy if model.pane.0 == Pane::Projects => {
                if let Some(row) = model.selected_row().cloned() {
                    model.dialog = Some(Dialog {
                        title: format!("Destroy {}", screen::cut(&row.name, 30)),
                        lines: vec![
                            "This removes the sandbox and everything in it,".to_owned(),
                            "including its database and uploads.".to_owned(),
                            String::new(),
                            "This cannot be undone.".to_owned(),
                        ],
                        footer: None,
                    });
                    confirming = Some(true);
                }
            }
            Key::Help => model.message = Some(view::help_for(model.pane.0).to_owned()),
            Key::Start
            | Key::Stop
            | Key::Destroy
            | Key::PreviousService
            | Key::NextService
            | Key::Follow
            | Key::Shell
            | Key::Import
            | Key::Pull
            | Key::Push
            | Key::PageUp
            | Key::PageDown
            | Key::Yes
            | Key::No
            | Key::Other => {}
        }
    }
    drop(raw);
    exit::OK
}

fn draw(deps: &mut Deps, model: &Model) {
    let (width, height) = term::size(deps.env, deps.input, deps.out);
    let rows = view::render(model, width as usize, height as usize);
    let fixed = has_fixed_colours(deps);
    let mut out = String::from("\x1b[H");
    let last = rows.len().saturating_sub(1);
    for (index, row) in rows.iter().enumerate() {
        for span in row {
            if deps.color {
                out.push_str("\x1b[");
                out.push_str(&span.style.sequence(fixed));
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

/// Whether the terminal has the 256-colour table, whose colours themes leave alone.
fn has_fixed_colours(deps: &Deps) -> bool {
    let term = (deps.env)("TERM").unwrap_or_default();
    let colorterm = (deps.env)("COLORTERM").unwrap_or_default();
    term.to_string_lossy().contains("256color") || !colorterm.is_empty()
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
    logs::status(deps.daemon, path).ok()
}

/// Reads whatever the showing panel needs. This waits for the daemon, but only
/// up to the limits in [`logs`] and [`traffic`], so a slow sandbox cannot freeze
/// the interface. A log is followed only while its panel shows, so leaving it
/// ends the command inside the sandbox.
fn refresh_panel_if_showing(
    deps: &mut Deps,
    model: &mut Model,
    follow: &mut Option<logs::Follow>,
) {
    // Stopped first, so a new one never runs beside the old one.
    *follow = None;
    match model.pane.0 {
        Pane::Logs => *follow = refresh_logs(deps, model),
        Pane::Traffic => refresh_traffic(deps, model),
        Pane::Secrets => refresh_secrets(deps, model),
        Pane::Projects | Pane::Review => {}
    }
}

/// Reads the selected project's secret names. This reads a small file in the
/// state folder and asks nothing of the daemon, so it needs no limit.
fn refresh_secrets(deps: &mut Deps, model: &mut Model) {
    match model.selected_row().cloned() {
        Some(row) => secrets::refresh(deps.env, &mut model.secrets, &row.name),
        None => model.secrets = view::SecretsPanel::default(),
    }
}

fn refresh_traffic(deps: &mut Deps, model: &mut Model) {
    let Some(row) = model.selected_row().cloned() else {
        model.traffic = view::TrafficPanel::default();
        return;
    };
    model.traffic.belong_to(&row.path);
    model.busy = Some("Reading blocked hostnames.".to_owned());
    draw(deps, model);
    traffic::refresh(deps.daemon, &mut model.traffic, &row.path);
    model.busy = None;
}

fn refresh_logs(deps: &mut Deps, model: &mut Model) -> Option<logs::Follow> {
    let Some(row) = model.selected_row().cloned() else {
        model.logs = view::LogsPanel::default();
        return None;
    };
    model.logs.belong_to(&row.path);
    model.busy = Some("Reading logs.".to_owned());
    draw(deps, model);
    let follow = logs::follow(deps.daemon, &mut model.logs, &row.path);
    model.busy = None;
    follow
}

fn show_selected(deps: &mut Deps, model: &mut Model) {
    model.detail.clear();
    model.overview = None;
    let Some(row) = model.selected_row().cloned() else {
        return;
    };
    model.overview = Some(match logs::status(deps.daemon, &row.path) {
        Ok(report) => overview::build(&report),
        Err(error) => view::Overview {
            path: crate::ui::esc(&row.path),
            problem: Some(logs::failure(&error).text),
            ..view::Overview::default()
        },
    });
}

/// How long to wait for the daemon's say-so before a shell. The sandbox has
/// already been seen running, so this is only a slow daemon.
const SHELL_LIMIT: Duration = Duration::from_secs(15);

/// Opens a shell for the selected project, on the person's own terminal, the
/// way `shell` does: into the service chosen on the Logs panel, if there is
/// one, or the sandbox itself. The interface steps aside while it runs and
/// comes back when it ends.
fn open_shell(deps: &mut Deps, model: &mut Model, raw: &term::RawMode) {
    let Some(row) = model.selected_row().cloned() else {
        return;
    };
    // Asked here, where a refusal can still be shown on this screen, rather
    // than after it has gone.
    match logs::status(deps.daemon, &row.path) {
        Ok(report) if report.sandbox == SandboxState::Running => {}
        Ok(_) => {
            model.message = Some(
                "There is no running sandbox to open a shell in. Start it from Projects with 2."
                    .to_owned(),
            );
            return;
        }
        Err(error) => {
            model.message = Some(logs::failure(&error).text);
            return;
        }
    }
    let service = if model.logs.project == row.path {
        model.logs.chosen.clone()
    } else {
        None
    };
    let engine = match debug::prepare_shell(
        service.as_deref(),
        std::path::Path::new(&row.path),
        SHELL_LIMIT,
        deps,
    ) {
        Ok(engine) => engine,
        Err(refusal) => {
            model.message = Some(format!("No shell: {}", refusal.reason));
            return;
        }
    };
    raw.pause();
    let code = debug::run_engine("shell", engine, true, deps);
    raw.resume();
    let place = service.map_or_else(|| "the sandbox".to_owned(), |name| screen::cut(&name, 20));
    model.message = Some(format!("The shell in {place} ended with exit code {code}."));
}

/// Builds a plan and shows it in a box, so that Yes agrees to something seen.
/// What a keystroke did to a line being typed.
#[derive(Debug, PartialEq, Eq)]
enum LineEdit {
    Typing,
    Submit,
    Cancel,
}

const MAX_URL: usize = 300;

/// Applies typed or pasted bytes to a line. Only printable ASCII is kept,
/// which is all an SSH remote can hold. A lone Esc or Ctrl-C cancels; other
/// escape sequences, such as arrow keys, are ignored.
fn edit_line(text: &mut String, bytes: &[u8]) -> LineEdit {
    if bytes == b"\x1b" {
        return LineEdit::Cancel;
    }
    if bytes.first() == Some(&0x1b) {
        return LineEdit::Typing;
    }
    for &byte in bytes {
        match byte {
            b'\r' | b'\n' => return LineEdit::Submit,
            0x03 => return LineEdit::Cancel,
            0x7f | 0x08 => {
                text.pop();
            }
            0x15 => text.clear(),
            0x21..=0x7e if text.len() < MAX_URL => text.push(byte as char),
            _ => {}
        }
    }
    LineEdit::Typing
}

fn import_dialog(text: &str) -> Dialog {
    Dialog {
        title: "Import from Git".to_owned(),
        lines: vec![
            "An SSH remote, such as git@github.com:org/repo.git".to_owned(),
            String::new(),
            format!("> {text}_"),
            String::new(),
            "Fetched here with your SSH key, then you see the plan.".to_owned(),
            "The key never enters the sandbox.".to_owned(),
        ],
        footer: Some("Enter fetches   Esc cancels".to_owned()),
    }
}

/// Fetches a remote, adds it to the list, and proposes starting it, which
/// shows the plan and waits for a yes like any other start.
fn import(deps: &mut Deps, model: &mut Model, url: &str) -> Option<Pending> {
    if url.is_empty() {
        model.message = Some("Nothing was imported.".to_owned());
        return None;
    }
    model.busy = Some("Fetching on this machine. Your SSH agent may ask you to approve.".to_owned());
    draw(deps, model);
    let request = Request::Fetch(proto::FetchArgs {
        url: url.to_owned(),
        git_ref: None,
        fetch: true,
        ssh_auth_sock: crate::git_cmd::agent_socket(deps),
    });
    let answer = deps.daemon.call(request, &mut |_, _| {});
    model.busy = None;
    let report = match answer {
        Ok(Reply::Fetch(report)) => report,
        Ok(_) => return None,
        Err(error) => {
            model.message = Some(logs::failure(&error).text);
            return None;
        }
    };
    load_projects(deps, model);
    if let Some(index) = model.projects.iter().position(|row| row.path == report.project_path) {
        model.selected = index;
        show_selected(deps, model);
    }
    if !report.fetched {
        model.message = Some("Already in a sandbox. Press g to pull newer commits.".to_owned());
        return None;
    }
    propose_start(deps, model)
}

/// Pulls or pushes in the selected project and shows what Git said. Returns
/// whether a box is now showing.
fn git_on_selected(deps: &mut Deps, model: &mut Model, push: bool) -> bool {
    let Some(row) = model.selected_row().cloned() else {
        return false;
    };
    let verb = if push { "Pushing" } else { "Pulling" };
    model.busy = Some(format!("{verb} {}. Your SSH agent may ask you to approve.", screen::cut(&row.name, 30)));
    draw(deps, model);
    let op = if push { proto::GitOp::Push } else { proto::GitOp::Pull };
    let result = crate::git_cmd::call(std::path::Path::new(&row.path), op, deps);
    model.busy = None;
    let report = match result {
        Ok(report) => report,
        Err(message) => {
            model.message = Some(message);
            return false;
        }
    };
    let mut lines: Vec<String> = report.output.lines().map(str::to_owned).collect();
    if lines.len() > 10 {
        lines.drain(..lines.len() - 10);
    }
    for update in &report.updates {
        lines.push(format!("pushed {} {:.10} -> {:.10}", update.name, update.old, update.new));
    }
    if let Some(branch) = &report.branch {
        lines.push(format!("on branch {branch}"));
    }
    let title = match (&report.refused, report.ok) {
        (Some(_), _) => format!("{} refused", if push { "Push" } else { "Pull" }),
        (None, true) => format!("{} done", if push { "Push" } else { "Pull" }),
        (None, false) => format!("{} failed", if push { "Push" } else { "Pull" }),
    };
    if let Some(reason) = &report.refused {
        lines.push(String::new());
        lines.push(reason.clone());
    }
    model.dialog = Some(Dialog { title, lines, footer: Some("Press any key".to_owned()) });
    true
}

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
            model.message = Some(logs::failure(&error).text);
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
        footer: None,
    });
    Some(Pending {
        name: row.name,
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

/// The shortest time between two drawings of the start's progress. A build
/// can report many steps a second, and each drawing asks the terminal its size.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// Starts the project, showing each step the daemon reports in a box as it
/// arrives. This has no time limit, because a first build can take minutes,
/// and nothing else can happen until it ends.
fn start(deps: &mut Deps, model: &mut Model, pending: Pending) {
    model.busy = Some("Starting. A first build can take minutes.".to_owned());
    model.progress = Some(view::Progress::new(format!(
        "Starting {}",
        screen::cut(&pending.name, 30)
    )));
    draw(deps, model);
    // The client is taken out first, so that the steps can be drawn while the
    // call is still running.
    let client = deps.daemon;
    let mut drawn = Instant::now();
    let result = client.call(
        Request::Up(proto::UpArgs {
            project: pending.project,
            source: Source::default(),
            plan_digest: pending.digest,
            approve_ports: pending.ports,
        }),
        &mut |step, detail| {
            if let Some(progress) = &mut model.progress {
                progress.push(step, detail);
            }
            if drawn.elapsed() >= PROGRESS_EVERY {
                draw(deps, model);
                drawn = Instant::now();
            }
        },
    );
    model.busy = None;
    model.progress = None;
    model.message = Some(match result {
        Ok(Reply::Up(report)) => format!("Started in {}s.", report.seconds),
        Ok(_) => "The daemon answered something else.".to_owned(),
        Err(error) => logs::failure(&error).text,
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
        Err(error) => logs::failure(&error).text,
    });
    load_projects(deps, model);
}

#[cfg(test)]
mod import_tests {
    use super::*;

    #[test]
    fn a_remote_is_typed_pasted_edited_and_submitted() {
        let mut text = String::new();
        assert_eq!(edit_line(&mut text, b"git@github.com:org/rep"), LineEdit::Typing);
        assert_eq!(edit_line(&mut text, b"x\x7fo.git"), LineEdit::Typing);
        assert_eq!(text, "git@github.com:org/repo.git");
        assert_eq!(edit_line(&mut text, b"\r"), LineEdit::Submit);
        assert_eq!(edit_line(&mut text, b"\x1b[D"), LineEdit::Typing, "arrows are ignored");
        assert_eq!(text, "git@github.com:org/repo.git");
    }

    #[test]
    fn control_and_wide_characters_never_reach_the_line() {
        let mut text = String::new();
        edit_line(&mut text, "a b\tc\u{1b}d\u{e9}".as_bytes());
        assert!(text.chars().all(|c| c.is_ascii_graphic()), "{text:?}");
        assert_eq!(edit_line(&mut text, b"\x1b"), LineEdit::Cancel);
        assert_eq!(edit_line(&mut text, b"\x03"), LineEdit::Cancel);
        let mut long = String::new();
        edit_line(&mut long, &[b'a'; 1000]);
        assert_eq!(long.len(), MAX_URL);
    }

    #[test]
    fn the_import_box_says_where_the_key_stays_and_how_to_leave() {
        let dialog = import_dialog("git@h:o/r.git");
        assert!(dialog.lines.iter().any(|l| l.contains("> git@h:o/r.git_")));
        assert!(dialog.lines.iter().any(|l| l.contains("never enters the sandbox")));
        assert!(dialog.footer.as_deref().unwrap().contains("Esc"));
    }
}
