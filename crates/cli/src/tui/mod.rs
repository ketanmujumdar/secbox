//! The full-screen interface.
//!
//! It is one more client of the daemon, with no powers the command line
//! lacks: every action here becomes the same request, and the daemon applies
//! the same rules. It needs no web view and no front-end toolchain, which is
//! the reason to try it before a desktop app.
//!
//! With no project yet, the page offers the two ways to bring one in: `g`
//! clones an SSH remote and `f` adds a folder, offering the one it was started
//! in when that looks like a project. With projects, they are listed down a
//! rail on the left, `+` (or `N`) offers the same two ways again, and the
//! selected project has five tabs, each opened by the letter drawn on it:
//! `o` Overview, `l` Logs, `n` Network, `c` Changes and `s` Secrets. Keys that
//! act on a sandbox work only on Overview, and only where its state allows:
//! `u` starts, `S` stops and `x` destroys. Pull (`p`) and push (`P`) work only
//! on Changes. The bar along the bottom lists only the keys that work.
//!
//! Starting a project still needs a person to agree, so the start key opens a
//! box showing the plan, grouped as the review groups it, and waits. Only `y`
//! agrees, and only once the whole plan has been on screen; Enter means no.
//! Nothing is forwarded by a keystroke that did not see what it was agreeing
//! to.

mod logs;
mod overview;
mod plan;
mod review;
mod screen;
mod secrets;
mod style;
mod term;
mod traffic;
mod view;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use proto::{DownArgs, PlanArgs, Reply, Request, SandboxState, Source};

use crate::client::CallError;
use crate::{debug, exit, tell, Deps};
use style::Style;
pub(crate) use term::SavedSettings;
use view::{
    Answer, Choice, ChoiceKey, Daemon, Dialog, Key, Line, Model, NewProject, Row, RowState, Tab,
};

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
    // A box that only reports, closed by any key.
    let mut notice = false;
    // The log being followed, if the Logs tab is showing one.
    let mut follow: Option<logs::Follow> = None;
    let mut keyboard = term::Keyboard::default();
    // The size last drawn at, which is the size a question was seen at.
    let mut size = (0, 0);
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
            size = draw(deps, &model);
            if let Some(dialog) = model.dialog.as_mut() {
                dialog.note_seen(size.0, size.1);
            }
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

        // Typing a remote or a folder takes the bytes themselves, pastes
        // included. Whatever follows Enter in the same read is dropped, so it
        // cannot answer the question that Enter brings up.
        if let Some(new) = model.new_project.as_mut().filter(|new| new.typing) {
            let bytes = std::mem::take(&mut queued);
            let chosen = new.chosen;
            let (text, kind) = match chosen {
                Choice::Git => (&mut new.remote, LineKind::Remote),
                Choice::Folder => (&mut new.folder, LineKind::Path),
            };
            match edit_line(text, &bytes, kind) {
                LineEdit::Typing => {}
                LineEdit::Cancel => {
                    new.typing = false;
                    model.message = Some("Nothing was added.".to_owned());
                }
                LineEdit::Submit => {
                    new.typing = false;
                    let value = text.clone();
                    pending = match chosen {
                        Choice::Git => import(deps, &mut model, value.trim()),
                        Choice::Folder => add_folder(deps, &mut model, &value),
                    };
                }
            }
            continue;
        }
        let taken = view::first_key_len(&queued);
        let bytes: Vec<u8> = queued.drain(..taken).collect();

        if notice {
            notice = false;
            model.dialog = None;
            continue;
        }

        if confirming.is_some() || pending.is_some() {
            match view::answer(&bytes) {
                Answer::Yes if model.dialog.as_ref().is_some_and(|d| !d.seen_all) => {
                    model.message = Some("Read all of it first: down shows the rest.".to_owned());
                }
                Answer::Yes => {
                    model.dialog = None;
                    if let Some(destroy) = confirming.take() {
                        stop(deps, &mut model, destroy);
                    } else if let Some(waiting) = pending.take() {
                        start(deps, &mut model, waiting);
                    }
                    refresh_tab_if_showing(deps, &mut model, &mut follow);
                }
                Answer::No => {
                    model.dialog = None;
                    model.message = Some(
                        if pending.take().is_some() {
                            "Nothing was started."
                        } else {
                            "Nothing was changed."
                        }
                        .to_owned(),
                    );
                    confirming = None;
                }
                moved @ (Answer::Up | Answer::Down | Answer::PageUp | Answer::PageDown) => {
                    let delta = match moved {
                        Answer::Up => -1,
                        Answer::Down => 1,
                        Answer::PageUp => -LOG_PAGE,
                        _ => LOG_PAGE,
                    };
                    if let Some(dialog) = model.dialog.as_mut() {
                        dialog.scroll_by(delta, size.0, size.1);
                    }
                }
                Answer::Other => {}
            }
            continue;
        }

        if model.new_project.is_some() {
            match choose(deps, &mut model, &bytes) {
                Chose::Quit => break,
                Chose::Back => refresh_tab_if_showing(deps, &mut model, &mut follow),
                Chose::Stay => {}
            }
            continue;
        }

        let key = view::key(&bytes);
        let offers = model.offers();
        let tab = model.tab;
        match key {
            Key::Quit => break,
            Key::Up | Key::Down | Key::PageUp | Key::PageDown
                if matches!(tab, Tab::Logs | Tab::Network | Tab::Changes) =>
            {
                // Logs and Network count back from the newest; Changes reads
                // down from the top.
                let back = match key {
                    Key::Up => 1,
                    Key::Down => -1,
                    Key::PageUp => LOG_PAGE,
                    _ => -LOG_PAGE,
                };
                match tab {
                    Tab::Logs => model.logs.scroll_by(back),
                    Tab::Network => model.traffic.scroll_by(back),
                    _ => model.review.scroll_by(-back),
                }
            }
            Key::Up | Key::Down => {
                model.move_by(if key == Key::Up { -1 } else { 1 });
                show_selected(deps, &mut model);
                refresh_tab_if_showing(deps, &mut model, &mut follow);
            }
            Key::Follow if tab == Tab::Logs => model.logs.jump_to_newest(),
            Key::PreviousService | Key::NextService if tab == Tab::Logs => {
                model
                    .logs
                    .choose(if key == Key::NextService { 1 } else { -1 });
                refresh_tab_if_showing(deps, &mut model, &mut follow);
            }
            Key::Open(next) => open_tab(deps, &mut model, &mut follow, next),
            Key::NextTab => open_tab(deps, &mut model, &mut follow, tab.next()),
            Key::PreviousTab => open_tab(deps, &mut model, &mut follow, tab.previous()),
            // Comparing runs inside the sandbox, so it happens only when asked.
            Key::Refresh if tab == Tab::Changes => {
                load_projects(deps, &mut model);
                compare(deps, &mut model);
            }
            Key::Refresh => {
                load_projects(deps, &mut model);
                refresh_tab_if_showing(deps, &mut model, &mut follow);
            }
            // What acts on a sandbox works only on Overview, so a key pressed
            // while reading something else cannot stop one.
            Key::Start if tab == Tab::Overview && offers.start => {
                pending = propose_start(deps, &mut model);
            }
            Key::Stop if tab == Tab::Overview && offers.stop => {
                if let Some(row) = model.selected_row().cloned() {
                    model.dialog = Some(Dialog::question(
                        format!("Stop {}", screen::cut(&row.name, 30)),
                        vec![
                            Line::plain("Its services stop and their ports close."),
                            Line::plain("Its data is kept for the next start."),
                        ],
                    ));
                    confirming = Some(false);
                }
            }
            Key::Destroy if tab == Tab::Overview && offers.destroy => {
                if let Some(row) = model.selected_row().cloned() {
                    model.dialog = Some(Dialog::question(
                        format!("Destroy {}", screen::cut(&row.name, 30)),
                        vec![
                            Line::plain("This removes the sandbox and everything in it,"),
                            Line::plain("including its database and uploads."),
                            Line::plain(""),
                            Line::styled("This cannot be undone.", Style::WindowAlert),
                        ],
                    ));
                    confirming = Some(true);
                }
            }
            Key::Shell if matches!(tab, Tab::Overview | Tab::Logs) && offers.shell => {
                open_shell(deps, &mut model, &raw);
                // Whatever was typed for the shell was the shell's.
                queued.clear();
            }
            Key::Pull | Key::Push if tab == Tab::Changes => {
                if git_on_selected(deps, &mut model, key == Key::Push) {
                    notice = true;
                }
                queued.clear();
                refresh_tab_if_showing(deps, &mut model, &mut follow);
            }
            Key::NewProject => {
                follow = None;
                model.message = None;
                model.new_project = Some(new_project_page(deps));
            }
            Key::Help => model.message = Some(view::help_for(&model).to_owned()),
            Key::PageUp
            | Key::PageDown
            | Key::Follow
            | Key::PreviousService
            | Key::NextService
            | Key::Start
            | Key::Stop
            | Key::Destroy
            | Key::Shell
            | Key::Pull
            | Key::Push
            | Key::Other => {}
        }
    }
    drop(raw);
    exit::OK
}

/// What a key on the New project page led to.
enum Chose {
    Stay,
    /// Back to the selected project, whose tab may need reading again.
    Back,
    Quit,
}

/// A key on the page that offers the two ways in. Choosing one starts typing
/// into it; the folder line starts with the folder offered, if there is one.
fn choose(deps: &mut Deps, model: &mut Model, bytes: &[u8]) -> Chose {
    let has_projects = !model.projects.is_empty();
    let Some(new) = model.new_project.as_mut() else {
        return Chose::Stay;
    };
    let open = |new: &mut NewProject, choice: Choice| {
        new.chosen = choice;
        new.typing = true;
        if choice == Choice::Folder && new.folder.is_empty() {
            new.folder = new.offered.clone().unwrap_or_default();
        }
    };
    match view::choice_key(bytes) {
        ChoiceKey::Git => open(new, Choice::Git),
        ChoiceKey::Folder => open(new, Choice::Folder),
        ChoiceKey::Enter => {
            let chosen = new.chosen;
            open(new, chosen);
        }
        ChoiceKey::Left => new.chosen = Choice::Git,
        ChoiceKey::Right => new.chosen = Choice::Folder,
        ChoiceKey::Back if has_projects => {
            model.new_project = None;
            model.message = None;
            show_selected(deps, model);
            return Chose::Back;
        }
        ChoiceKey::Help => model.message = Some(view::help_for(model).to_owned()),
        ChoiceKey::Quit => return Chose::Quit,
        ChoiceKey::Back | ChoiceKey::Other => {}
    }
    Chose::Stay
}

fn open_tab(deps: &mut Deps, model: &mut Model, follow: &mut Option<logs::Follow>, tab: Tab) {
    model.tab = tab;
    model.message = None;
    refresh_tab_if_showing(deps, model, follow);
}

/// Draws the screen and returns the size it was drawn at.
fn draw(deps: &mut Deps, model: &Model) -> (usize, usize) {
    let (width, height) = term::size(deps.env, deps.input, deps.out);
    let (width, height) = (width as usize, height as usize);
    let rows = view::render(model, width, height);
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
    (width, height)
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
        if model.projects.is_empty() && model.new_project.is_none() {
            model.new_project = Some(new_project_page(deps));
        }
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
    let mut answered = false;
    let mut failed = false;
    for index in 0..model.projects.len() {
        let path = model.projects[index].path.clone();
        match logs::status(deps.daemon, &path) {
            Ok(report) => {
                answered = true;
                model.projects[index].state = match report.sandbox {
                    SandboxState::Running => RowState::Running,
                    SandboxState::Stopped => RowState::Stopped,
                    SandboxState::None => RowState::None,
                    SandboxState::Unknown => RowState::Unknown,
                };
            }
            Err(CallError::Refused(_)) => answered = true,
            Err(_) => failed = true,
        }
    }
    if model.projects.is_empty() {
        // An empty path is refused before the daemon touches anything, so
        // this asks only whether it is there.
        match logs::status(deps.daemon, "") {
            Ok(_) | Err(CallError::Refused(_)) => answered = true,
            Err(_) => failed = true,
        }
        if model.new_project.is_none() {
            model.new_project = Some(new_project_page(deps));
        }
    }
    model.daemon = match (answered, failed) {
        (true, _) => Daemon::Up,
        (false, true) => Daemon::Down,
        (false, false) => Daemon::Unknown,
    };
    show_selected(deps, model);
}

/// The New project page, with the folder this was started in offered when it
/// looks like a project, and chosen first then.
fn new_project_page(deps: &Deps) -> NewProject {
    let offered = offered_folder(deps);
    NewProject {
        chosen: if offered.is_some() {
            Choice::Folder
        } else {
            Choice::Git
        },
        offered,
        ..NewProject::default()
    }
}

/// The current folder, if it looks like a project: it has a Compose file, a
/// Dockerfile, a dev container or a package manifest. The home folder and the
/// root are never offered, whatever they hold.
fn offered_folder(deps: &Deps) -> Option<String> {
    let cwd = deps.cwd.as_path();
    let home = (deps.env)("HOME").map(PathBuf::from);
    if cwd == Path::new("/") || home.as_deref() == Some(cwd) {
        return None;
    }
    match projectcheck::detect::detect(cwd) {
        Ok(found) => match found.kind {
            projectcheck::detect::ProjectKind::Unknown
            | projectcheck::detect::ProjectKind::Unsupported { .. } => None,
            _ => Some(cwd.to_string_lossy().into_owned()),
        },
        Err(_) => None,
    }
}

/// Reads whatever the showing tab needs. This waits for the daemon, but only
/// up to the limits in [`logs`] and [`traffic`], so a slow sandbox cannot freeze
/// the interface. A log is followed only while its tab shows, so leaving it
/// ends the command inside the sandbox.
fn refresh_tab_if_showing(deps: &mut Deps, model: &mut Model, follow: &mut Option<logs::Follow>) {
    // Stopped first, so a new one never runs beside the old one.
    *follow = None;
    if model.new_project.is_some() {
        return;
    }
    match model.tab {
        Tab::Logs => *follow = refresh_logs(deps, model),
        Tab::Network => refresh_traffic(deps, model),
        Tab::Secrets => refresh_secrets(deps, model),
        Tab::Changes => match model.selected_row() {
            Some(row) => {
                let path = row.path.clone();
                model.review.belong_to(&path);
            }
            None => model.review = view::ReviewPanel::default(),
        },
        Tab::Overview => {}
    }
}

/// Compares the selected project's sandbox with its snapshot, into the
/// Changes tab.
fn compare(deps: &mut Deps, model: &mut Model) {
    let Some(row) = model.selected_row().cloned() else {
        model.review = view::ReviewPanel::default();
        return;
    };
    model.busy = Some("Comparing inside the sandbox. A large project can take a while.".to_owned());
    draw(deps, model);
    review::refresh(deps.daemon, &mut model.review, &row.path);
    model.busy = None;
}

/// Reads the selected project's secret names. This reads a small file in the
/// state folder and asks nothing of the daemon, so it needs no limit.
fn refresh_secrets(deps: &mut Deps, model: &mut Model) {
    match model.selected_row().cloned() {
        Some(row) => {
            secrets::refresh(deps.env, &mut model.secrets, &row.name);
            model.secrets.reach = logs::status(deps.daemon, &row.path)
                .ok()
                .and_then(|report| report.exposure)
                .map(|e| {
                    proto::exposure_lines(&e.secrets, e.reachable_hosts.as_deref(), true)
                        .iter()
                        .map(|line| crate::ui::esc(line))
                        .collect()
                })
                .unwrap_or_default();
        }
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
/// way `shell` does: into the service chosen on the Logs tab, if there is
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
                "There is no running sandbox to open a shell in. Start it with u on Overview."
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

/// What a keystroke did to a line being typed.
#[derive(Debug, PartialEq, Eq)]
enum LineEdit {
    Typing,
    Submit,
    Cancel,
}

/// What a line holds, which decides what can be typed into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    /// An SSH remote: printable ASCII, no spaces.
    Remote,
    /// A folder: anything but control characters.
    Path,
}

const MAX_URL: usize = 300;
const MAX_PATH: usize = 4096;

/// Applies typed or pasted bytes to a line. A remote keeps only printable
/// ASCII, which is all one can hold; a folder keeps any character but a
/// control code. A lone Esc or Ctrl-C cancels; other escape sequences, such
/// as arrow keys, are ignored.
fn edit_line(text: &mut String, bytes: &[u8], kind: LineKind) -> LineEdit {
    if bytes == b"\x1b" {
        return LineEdit::Cancel;
    }
    if bytes.first() == Some(&0x1b) {
        return LineEdit::Typing;
    }
    let most = match kind {
        LineKind::Remote => MAX_URL,
        LineKind::Path => MAX_PATH,
    };
    for ch in String::from_utf8_lossy(bytes).chars() {
        match ch {
            '\r' | '\n' => return LineEdit::Submit,
            '\u{3}' => return LineEdit::Cancel,
            '\u{7f}' | '\u{8}' => {
                text.pop();
            }
            '\u{15}' => text.clear(),
            c if text.len() + c.len_utf8() > most => {}
            c if kind == LineKind::Remote && c.is_ascii_graphic() => text.push(c),
            c if kind == LineKind::Path && !c.is_control() && c != char::REPLACEMENT_CHARACTER => {
                text.push(c);
            }
            _ => {}
        }
    }
    LineEdit::Typing
}

/// A leading `~` is the home folder, as a shell would read it.
fn expand_home(text: &str, env: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> PathBuf {
    let home = || env("HOME").map(PathBuf::from);
    match text.strip_prefix('~') {
        Some("") => home().unwrap_or_else(|| PathBuf::from(text)),
        Some(rest) if rest.starts_with('/') => home()
            .map(|home| home.join(rest.trim_start_matches('/')))
            .unwrap_or_else(|| PathBuf::from(text)),
        _ => PathBuf::from(text),
    }
}

/// Adds a folder: works out its plan, which also lists it, and proposes
/// starting it, which shows the plan and waits for a yes like any other start.
fn add_folder(deps: &mut Deps, model: &mut Model, text: &str) -> Option<Pending> {
    let text = text.trim();
    if text.is_empty() {
        model.message = Some("Nothing was added.".to_owned());
        return None;
    }
    let path = expand_home(text, deps.env);
    let root = match crate::project_dir(Some(&path), deps) {
        Ok(root) => root,
        Err(reason) => {
            model.message = Some(format!("That folder cannot be added: {reason}"));
            return None;
        }
    };
    propose_start_for(deps, model, &root.to_string_lossy(), true)
}

/// Fetches a remote, adds it to the list, and proposes starting it, which
/// shows the plan and waits for a yes like any other start.
fn import(deps: &mut Deps, model: &mut Model, url: &str) -> Option<Pending> {
    if url.is_empty() {
        model.message = Some("Nothing was imported.".to_owned());
        return None;
    }
    model.busy =
        Some("Fetching on this machine. Your SSH agent may ask you to approve.".to_owned());
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
    if let Some(index) = model
        .projects
        .iter()
        .position(|row| row.path == report.project_path)
    {
        model.selected = index;
        model.new_project = None;
        model.tab = Tab::Overview;
        show_selected(deps, model);
    }
    if !report.fetched {
        model.message =
            Some("Already in a sandbox. Press p on Changes to pull newer commits.".to_owned());
        return None;
    }
    // The plan is asked for by the path the daemon fetched into, so the box
    // can only ever show the project that was just fetched.
    propose_start_for(deps, model, &report.project_path, false)
}

/// Pulls or pushes in the selected project and shows what Git said. Returns
/// whether a box is now showing.
fn git_on_selected(deps: &mut Deps, model: &mut Model, push: bool) -> bool {
    let Some(row) = model.selected_row().cloned() else {
        return false;
    };
    let verb = if push { "Pushing" } else { "Pulling" };
    model.busy = Some(format!(
        "{verb} {}. Your SSH agent may ask you to approve.",
        screen::cut(&row.name, 30)
    ));
    draw(deps, model);
    let op = if push {
        proto::GitOp::Push
    } else {
        proto::GitOp::Pull
    };
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
        lines.push(format!(
            "pushed {} {:.10} -> {:.10}",
            update.name, update.old, update.new
        ));
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
    model.dialog = Some(Dialog::notice(
        title,
        lines.into_iter().map(Line::plain).collect(),
        "Press any key",
    ));
    true
}

/// Proposes starting the selected project.
fn propose_start(deps: &mut Deps, model: &mut Model) -> Option<Pending> {
    let row = model.selected_row().cloned()?;
    propose_start_for(deps, model, &row.path, false)
}

/// Builds a plan and shows it in a box, so that Yes agrees to something seen.
/// For a project not listed yet, asking for its plan is what lists it, so the
/// list is read again and the project chosen.
fn propose_start_for(deps: &mut Deps, model: &mut Model, path: &str, new: bool) -> Option<Pending> {
    model.busy = Some("Working out the plan.".to_owned());
    draw(deps, model);
    let answer = deps.daemon.call(
        Request::Plan(PlanArgs {
            project: path.to_owned(),
            source: Source::default(),
        }),
        &mut |_, _| {},
    );
    model.busy = None;
    let plan = match answer {
        Ok(Reply::Plan(plan)) => *plan,
        Ok(_) => return None,
        Err(error) => {
            model.message = Some(logs::failure(&error).text);
            return None;
        }
    };
    if new {
        load_projects(deps, model);
        if let Some(index) = model.projects.iter().position(|row| row.path == path) {
            model.selected = index;
            show_selected(deps, model);
        }
        if !model.projects.is_empty() {
            model.new_project = None;
            model.tab = Tab::Overview;
        }
    }
    let name = model
        .selected_row()
        .filter(|row| row.path == path)
        .map_or_else(|| crate::ui::esc(&plan.project_id), |row| row.name.clone());
    if !plan.blockers.is_empty() {
        model.detail = plan
            .blockers
            .iter()
            .map(|b| Line::styled(crate::ui::esc(b), Style::WindowAlert))
            .collect();
        model.message = Some("This cannot be started. See Overview.".to_owned());
        return None;
    }
    let ports: Vec<u16> = plan.ports.iter().map(|p| p.host_port).collect();
    model.dialog = Some(Dialog::question(
        format!("Start {}", screen::cut(&name, 30)),
        plan::start_lines(&plan),
    ));
    Some(Pending {
        name,
        project: path.to_owned(),
        digest: plan.digest,
        ports,
    })
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
    // Reading the list again clears the line, so what happened is kept.
    let said = model.message.take();
    load_projects(deps, model);
    model.message = said;
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
    let said = match result {
        Ok(Reply::Down(report)) if report.destroyed => {
            "Destroyed. Rotate every secret it was given.".to_owned()
        }
        Ok(Reply::Down(_)) => "Stopped. Its data is kept for the next start.".to_owned(),
        Ok(_) => "The daemon answered something else.".to_owned(),
        Err(error) => logs::failure(&error).text,
    };
    load_projects(deps, model);
    model.message = Some(said);
}

#[cfg(test)]
mod import_tests {
    use super::*;

    #[test]
    fn a_remote_is_typed_pasted_edited_and_submitted() {
        let mut text = String::new();
        let remote = LineKind::Remote;
        assert_eq!(
            edit_line(&mut text, b"git@github.com:org/rep", remote),
            LineEdit::Typing
        );
        assert_eq!(
            edit_line(&mut text, b"x\x7fo.git", remote),
            LineEdit::Typing
        );
        assert_eq!(text, "git@github.com:org/repo.git");
        assert_eq!(edit_line(&mut text, b"\r", remote), LineEdit::Submit);
        assert_eq!(
            edit_line(&mut text, b"\x1b[D", remote),
            LineEdit::Typing,
            "arrows are ignored"
        );
        assert_eq!(text, "git@github.com:org/repo.git");
    }

    #[test]
    fn control_and_wide_characters_never_reach_a_remote() {
        let mut text = String::new();
        edit_line(
            &mut text,
            "a b\tc\u{1b}d\u{e9}".as_bytes(),
            LineKind::Remote,
        );
        assert!(text.chars().all(|c| c.is_ascii_graphic()), "{text:?}");
        assert_eq!(
            edit_line(&mut text, b"\x1b", LineKind::Remote),
            LineEdit::Cancel
        );
        assert_eq!(
            edit_line(&mut text, b"\x03", LineKind::Remote),
            LineEdit::Cancel
        );
        let mut long = String::new();
        edit_line(&mut long, &[b'a'; 1000], LineKind::Remote);
        assert_eq!(long.len(), MAX_URL);
    }

    /// A folder can hold spaces and letters from any language, but never a
    /// control code, which could otherwise reach the terminal.
    #[test]
    fn a_folder_keeps_spaces_and_letters_but_no_control_codes() {
        let mut text = String::new();
        edit_line(
            &mut text,
            "~/My Projects/caf\u{e9}\u{7}\u{9b}x".as_bytes(),
            LineKind::Path,
        );
        assert_eq!(text, "~/My Projects/caf\u{e9}x");
        assert!(text.chars().all(|c| !c.is_control()), "{text:?}");
        edit_line(&mut text, b"\x15", LineKind::Path);
        assert!(text.is_empty());
        let mut long = String::new();
        edit_line(&mut long, &[b'a'; 10_000], LineKind::Path);
        assert_eq!(long.len(), MAX_PATH);
    }

    /// Enter submits, and whatever came after it in the same read is not
    /// typed, so a paste cannot carry on into the question Enter brings up.
    #[test]
    fn what_follows_enter_in_a_paste_is_not_kept() {
        let mut text = String::new();
        assert_eq!(
            edit_line(&mut text, b"/srv/app\ry", LineKind::Path),
            LineEdit::Submit
        );
        assert_eq!(text, "/srv/app");
    }

    #[test]
    fn a_leading_tilde_is_the_home_folder() {
        let env = |name: &str| (name == "HOME").then(|| std::ffi::OsString::from("/home/ada"));
        assert_eq!(
            expand_home("~/code/app", &env),
            PathBuf::from("/home/ada/code/app")
        );
        assert_eq!(expand_home("~", &env), PathBuf::from("/home/ada"));
        assert_eq!(expand_home("/srv/~x", &env), PathBuf::from("/srv/~x"));
        assert_eq!(expand_home("~bob/x", &env), PathBuf::from("~bob/x"));
    }
}
