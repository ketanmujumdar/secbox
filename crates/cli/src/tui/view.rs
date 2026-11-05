//! What the interface looks like, worked out as a grid of coloured cells.
//!
//! The look is deliberate: a menu bar across the top, a patterned desktop, a
//! double-ruled window casting a shadow, a scrollbar down the side, and a
//! numbered function-key bar along the bottom. That is how a full-screen
//! program looked when terminals had sixteen colours, and it is the reason
//! this needs no web view.
//!
//! This module touches no terminal. It takes a model and a size and returns
//! the cells, which is what makes the layout testable. Every string in it
//! that came from a project or a sandbox arrives already escaped, because it
//! came through the daemon; cutting it to the width is the only other thing
//! needed to keep a hostile name from breaking the frame.

use super::screen::{cut, Rect, Screen, Span};
use super::style::Style;

/// Which panel is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Projects,
    Traffic,
    Secrets,
    Review,
    Logs,
}

impl Pane {
    pub const ALL: [Pane; 5] = [
        Pane::Projects,
        Pane::Traffic,
        Pane::Secrets,
        Pane::Review,
        Pane::Logs,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Pane::Projects => "Projects",
            Pane::Traffic => "Traffic",
            Pane::Secrets => "Secrets",
            Pane::Review => "Review",
            Pane::Logs => "Logs",
        }
    }

    pub fn next(self) -> Self {
        let all = Self::ALL;
        let at = all.iter().position(|p| *p == self).unwrap_or(0);
        all[(at + 1) % all.len()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    Running,
    Stopped,
    None,
    Unknown,
}

impl RowState {
    fn word(self) -> &'static str {
        match self {
            RowState::Running => "running",
            RowState::Stopped => "stopped",
            RowState::None => "—",
            RowState::Unknown => "?",
        }
    }

    fn style(self) -> Style {
        match self {
            RowState::Running => Style::WindowGood,
            RowState::Stopped => Style::WindowWarn,
            RowState::None => Style::WindowDim,
            RowState::Unknown => Style::WindowAlert,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub path: String,
    pub state: RowState,
}

/// A line in the lower half of the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub style: Style,
}

impl Line {
    pub fn dim(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: Style::WindowDim,
        }
    }

    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: Style::Window,
        }
    }

    pub fn styled(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }
}

/// A question waiting for an answer, drawn over everything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    pub title: String,
    pub lines: Vec<String>,
}

/// Whether new lines are arriving by themselves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Live {
    /// Read once, so nothing more will arrive until it is read again.
    #[default]
    Off,
    /// New lines are added as the sandbox writes them.
    Following,
    /// Following stopped, for the reason in the note.
    Ended,
}

/// What the Logs panel shows for the selected project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogsPanel {
    /// The project all of this belongs to.
    pub project: String,
    /// The services that can be chosen. All of them is always a choice too.
    pub services: Vec<String>,
    /// The chosen service, or `None` for all of them.
    pub chosen: Option<String>,
    /// The most recent lines, oldest first, filtered and with no tabs left.
    pub lines: Vec<String>,
    /// Why there are no lines, or something else worth saying about them.
    pub note: Option<Line>,
    /// How many lines back from the newest the view has been scrolled. Above
    /// zero, following is paused: new lines are kept but the view stays put.
    pub scroll: usize,
    pub live: Live,
    /// Lines that arrived while paused, so the person knows there are some.
    pub unseen: usize,
}

impl LogsPanel {
    /// Clears everything if it belongs to another project, so that one
    /// project's lines are never drawn under another's name while reading.
    pub fn belong_to(&mut self, project: &str) {
        if self.project != project {
            *self = Self {
                project: project.to_owned(),
                ..Self::default()
            };
        }
    }

    /// Moves along All and the services, stopping at each end.
    pub fn choose(&mut self, delta: isize) {
        let at = self.chosen_index();
        let next = at.saturating_add_signed(delta).min(self.services.len());
        self.chosen = next.checked_sub(1).map(|index| self.services[index].clone());
        self.jump_to_newest();
    }

    /// Moves back through the lines, or forward towards the newest. It stops
    /// at the oldest line, so one key press back always moves something.
    /// Reaching the newest line again resumes following.
    pub fn scroll_by(&mut self, delta: isize) {
        let furthest = self.lines.len().saturating_sub(1);
        self.scroll = self.scroll.saturating_add_signed(delta).min(furthest);
        if self.scroll == 0 {
            self.unseen = 0;
        }
    }

    /// Jumps to the newest line, which resumes following.
    pub fn jump_to_newest(&mut self) {
        self.scroll = 0;
        self.unseen = 0;
    }

    pub fn paused(&self) -> bool {
        self.scroll > 0
    }

    /// Adds one line at the newest end. While paused the view is moved back by
    /// one too, so what the person is reading does not slide away under them.
    pub fn push_line(&mut self, line: String) {
        self.lines.push(line);
        if self.paused() {
            self.scroll += 1;
            self.unseen += 1;
        }
    }

    /// Drops the oldest lines beyond `keep`. The view counts from the newest,
    /// so it only moves if it was among the lines dropped.
    pub fn trim(&mut self, keep: usize) {
        let over = self.lines.len().saturating_sub(keep);
        if over > 0 {
            self.lines.drain(..over);
            self.scroll = self.scroll.min(self.lines.len().saturating_sub(1));
        }
    }

    /// Where the choice is along All and the services, with All first.
    fn chosen_index(&self) -> usize {
        self.chosen
            .as_ref()
            .and_then(|name| self.services.iter().position(|s| s == name))
            .map_or(0, |index| index + 1)
    }
}

/// What the Traffic panel shows for the selected project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrafficPanel {
    /// The project all of this belongs to.
    pub project: String,
    /// What has to be said about the list. Never scrolled away.
    pub notes: Vec<Line>,
    /// One blocked hostname per row, the newest last.
    pub rows: Vec<String>,
    /// How many rows back from the newest the view has been scrolled.
    pub scroll: usize,
}

impl TrafficPanel {
    /// Clears everything if it belongs to another project, so that one
    /// project's list is never drawn under another's name while reading.
    pub fn belong_to(&mut self, project: &str) {
        if self.project != project {
            *self = Self {
                project: project.to_owned(),
                ..Self::default()
            };
        }
    }

    /// Moves back through the rows, or forward towards the newest, stopping at
    /// the oldest so that one key press back always moves something.
    pub fn scroll_by(&mut self, delta: isize) {
        let furthest = self.rows.len().saturating_sub(1);
        self.scroll = self.scroll.saturating_add_signed(delta).min(furthest);
    }
}

#[derive(Debug, Clone, Default)]
pub struct Model {
    pub projects: Vec<Row>,
    pub selected: usize,
    pub detail: Vec<Line>,
    pub message: Option<String>,
    pub busy: Option<String>,
    pub pane: PaneState,
    pub dialog: Option<Dialog>,
    pub logs: LogsPanel,
    pub traffic: TrafficPanel,
}

#[derive(Debug, Clone, Copy)]
pub struct PaneState(pub Pane);

impl Default for PaneState {
    fn default() -> Self {
        Self(Pane::Projects)
    }
}

impl Model {
    pub fn selected_row(&self) -> Option<&Row> {
        self.projects.get(self.selected)
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.projects.is_empty() {
            return;
        }
        let last = self.projects.len() - 1;
        self.selected = match delta {
            d if d < 0 => self.selected.saturating_sub(d.unsigned_abs()),
            d => (self.selected + d as usize).min(last),
        };
    }
}

/// What each panel says when it has nothing to show, because the thing it
/// would show is not built. Saying so is the point: an empty panel would
/// otherwise read as "nothing happened".
fn not_built(pane: Pane) -> &'static [&'static str] {
    match pane {
        Pane::Projects | Pane::Logs | Pane::Traffic => &[],
        Pane::Secrets => &[
            "Secrets are stored, and assigned ones are delivered on each start.",
            "",
            "Use the secrets command to add and assign them. A service can read",
            "what it is given. A secret held by the proxy also needs the gateway",
            "to terminate TLS, which is not settled, so a plan with one is refused.",
        ],
        Pane::Review => &[
            "Reviewing changes out of a sandbox is not built.",
            "",
            "The broker that quarantines a bundle, flags risky paths and pushes",
            "one approved commit exists, but no command reaches it yet.",
        ],
    }
}

/// The bar along the bottom. A number and a word, as every program of the
/// period had.
const KEYS: [(&str, &str); 6] = [
    ("1", "Help"),
    ("2", "Start"),
    ("3", "Stop"),
    ("5", "Refresh"),
    ("6", "Panel"),
    ("10", "Quit"),
];

const MIN_WIDTH: usize = 60;
const MIN_HEIGHT: usize = 14;

/// Draws the whole screen.
pub fn render(model: &Model, width: usize, height: usize) -> Vec<Vec<Span>> {
    compose(model, width, height).rows()
}

/// The same drawing, as a grid, so a test can read it as plain text.
pub fn compose(model: &Model, width: usize, height: usize) -> Screen {
    let width = width.max(MIN_WIDTH);
    let height = height.max(MIN_HEIGHT);
    // The pattern behind everything, which is what makes a window read as a
    // window rather than as the whole screen.
    let mut screen = Screen::new(width, height, '▒', Style::Desktop);

    menu_bar(&mut screen, model.pane.0);
    status_bar(&mut screen);

    let window = Rect::new(1, 1, width - 4, height - 4);
    screen.shadow(window);
    screen.fill(window.x, window.y, window.w, window.h, ' ', Style::Window);
    screen.frame(window, Style::Frame, model.pane.0.label(), Style::Title);

    let inner_x = window.x + 1;
    let inner_y = window.y + 1;
    let inner_w = window.w - 2;
    let inner_h = window.h - 2;
    body(model, &mut screen, inner_x, inner_y, inner_w, inner_h);

    // The last line inside the window always carries whatever matters most.
    let note_y = inner_y + inner_h - 1;
    let (note, style) = match (&model.busy, &model.message) {
        (Some(busy), _) => (busy.clone(), Style::WindowWarn),
        (None, Some(message)) => (message.clone(), Style::WindowWarn),
        (None, None) => (
            "Early build. Do not rely on it for untrusted code.".to_owned(),
            Style::WindowAlert,
        ),
    };
    screen.fill(inner_x, note_y, inner_w, 1, ' ', style);
    screen.text_cut(inner_x + 1, note_y, &note, inner_w - 2, style);

    if let Some(dialog) = &model.dialog {
        draw_dialog(&mut screen, dialog);
    }
    screen
}

fn menu_bar(screen: &mut Screen, active: Pane) {
    screen.fill(0, 0, screen.width(), 1, ' ', Style::MenuBar);
    let mut x = 1;
    for pane in Pane::ALL {
        let label = pane.label();
        let style = if pane == active {
            Style::MenuActive
        } else {
            Style::MenuBar
        };
        let hot = if pane == active {
            Style::MenuActive
        } else {
            Style::MenuHot
        };
        screen.put(x, 0, ' ', style);
        // The first letter is the one that chooses it, so it is marked.
        screen.text(x + 1, 0, &label[..1], hot);
        screen.text(x + 2, 0, &label[1..], style);
        screen.put(x + 1 + label.len(), 0, ' ', style);
        x += label.chars().count() + 3;
    }
}

fn status_bar(screen: &mut Screen) {
    let y = screen.height() - 1;
    screen.fill(0, y, screen.width(), 1, ' ', Style::StatusLabel);
    let mut x = 1;
    for (number, word) in KEYS {
        if x + number.len() + word.len() + 2 >= screen.width() {
            break;
        }
        x += screen.text(x, y, number, Style::StatusKey);
        x += screen.text(x, y, word, Style::StatusLabel);
        x += 2;
    }
}

fn body(model: &Model, screen: &mut Screen, x: usize, y: usize, w: usize, h: usize) {
    // One line at the bottom is always the note.
    let room = h.saturating_sub(1);
    if model.pane.0 == Pane::Logs {
        logs_body(model, screen, x, y, w, room);
        return;
    }
    if model.pane.0 == Pane::Traffic {
        traffic_body(model, screen, x, y, w, room);
        return;
    }
    if model.pane.0 != Pane::Projects {
        for (offset, text) in not_built(model.pane.0).iter().take(room).enumerate() {
            screen.text_cut(x + 1, y + offset, text, w - 2, Style::Window);
        }
        return;
    }
    if model.projects.is_empty() {
        screen.text_cut(
            x + 1,
            y,
            "No project has been started on this machine.",
            w - 2,
            Style::Window,
        );
        screen.text_cut(
            x + 1,
            y + 2,
            "Run the up command in a project folder first.",
            w - 2,
            Style::WindowDim,
        );
        return;
    }

    // At least three rows, so the list always has a scrollbar to sit beside
    // and does not jump about as projects come and go.
    let list_h = (room / 2).clamp(3, room.max(1));
    let first = model.selected.saturating_sub(list_h.saturating_sub(1));
    let bar_x = x + w - 1;
    for offset in 0..list_h {
        let Some(row) = model.projects.get(first + offset) else {
            break;
        };
        let selected = first + offset == model.selected;
        let line_y = y + offset;
        let line_w = w - 1;
        if selected {
            screen.fill(x, line_y, line_w, 1, ' ', Style::Selected);
        }
        let name_style = if selected { Style::Selected } else { Style::Window };
        let state_style = if selected {
            Style::Selected
        } else {
            row.state.style()
        };
        let word = row.state.word();
        let name_room = line_w.saturating_sub(word.chars().count() + 4);
        screen.text(x + 1, line_y, &cut(&row.name, name_room), name_style);
        screen.text(
            x + line_w - word.chars().count() - 1,
            line_y,
            word,
            state_style,
        );
    }
    screen.scrollbar(bar_x, y, list_h, first, list_h, model.projects.len());

    let mut at = y + list_h;
    if at < y + room {
        screen.rule(x - 1, at, w + 2, Style::Frame);
        at += 1;
    }
    for line in &model.detail {
        if at >= y + room {
            break;
        }
        screen.text_cut(x + 1, at, &line.text, w - 2, line.style);
        at += 1;
    }
}

/// The services along the top, then as many of the most recent lines as fit.
fn logs_body(model: &Model, screen: &mut Screen, x: usize, y: usize, w: usize, room: usize) {
    let panel = &model.logs;
    let Some(row) = model.selected_row() else {
        screen.text_cut(
            x + 1,
            y,
            "No project has been started on this machine.",
            w - 2,
            Style::Window,
        );
        return;
    };
    // Whether lines are arriving is set at the right, where it stays in view
    // however the lines are scrolled; the name gets what room is left.
    let (tag, tag_style) = live_tag(panel);
    let tag_w = tag.chars().count().min(w - 2);
    screen.text_cut(
        x + 1,
        y,
        &row.name,
        (w - 2).saturating_sub(tag_w + 2),
        Style::WindowDim,
    );
    screen.text_cut(x + w - 1 - tag_w, y, &tag, tag_w, tag_style);
    service_bar(panel, screen, x + 1, y + 1, w - 2);
    screen.text_cut(
        x + 1,
        y + 2,
        "<- -> service  Up Down scroll  f follow. Output can be wrong.",
        w - 2,
        Style::WindowDim,
    );
    if room <= 4 {
        return;
    }
    screen.rule(x - 1, y + 3, w + 2, Style::Frame);
    let mut at = y + 4;
    let bottom = y + room;
    if let Some(note) = &panel.note {
        screen.text_cut(x + 1, at, &note.text, w - 2, note.style);
        at += 1;
    }
    let fits = bottom.saturating_sub(at);
    let end = panel.lines.len().saturating_sub(panel.scroll);
    let first = end.saturating_sub(fits);
    for line in &panel.lines[first..end] {
        screen.text_cut(x + 1, at, line, w - 2, Style::Window);
        at += 1;
    }
}

/// Says whether the panel is following, and when it is paused, how to resume
/// and how much is waiting, so that a still screen is never mistaken for a
/// quiet sandbox.
fn live_tag(panel: &LogsPanel) -> (String, Style) {
    match panel.live {
        Live::Following if panel.paused() => (
            match panel.unseen {
                0 => " Paused. f follows ".to_owned(),
                n => format!(" Paused, {n} new. f follows "),
            },
            Style::Selected,
        ),
        Live::Following => ("Following".to_owned(), Style::WindowGood),
        Live::Ended => ("Not following".to_owned(), Style::WindowWarn),
        Live::Off => (String::new(), Style::WindowDim),
    }
}

/// Breaks text at spaces to fit a width. A word longer than that is left whole,
/// for the drawing to cut, since these words are all this program's own.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// What the list cannot show comes first and stays there, whatever the scroll,
/// and is wrapped rather than cut. The rows get whatever room is left.
fn traffic_body(model: &Model, screen: &mut Screen, x: usize, y: usize, w: usize, room: usize) {
    let panel = &model.traffic;
    let bottom = y + room;
    let mut at = y;
    let width = w - 2;
    let say = |screen: &mut Screen, at: &mut usize, text: &str, style: Style| {
        for piece in wrap(text, width) {
            if *at < bottom {
                screen.text_cut(x + 1, *at, &piece, width, style);
                *at += 1;
            }
        }
    };
    if let Some(row) = model.selected_row() {
        screen.text_cut(x + 1, at, &row.name, width, Style::WindowDim);
        at += 1;
    }
    // An alert first, then what is always true, which needs no answer from the
    // daemon and so stands even when there is none.
    for note in panel.notes.iter().filter(|n| n.style == Style::WindowAlert) {
        say(screen, &mut at, &note.text, note.style);
    }
    say(screen, &mut at, crate::blocked::HOSTNAMES_ONLY, Style::WindowWarn);
    for note in panel.notes.iter().filter(|n| n.style != Style::WindowAlert) {
        say(screen, &mut at, &note.text, note.style);
    }
    if model.selected_row().is_none() {
        say(
            screen,
            &mut at,
            "No project has been started on this machine.",
            Style::Window,
        );
        return;
    }
    if at + 2 > bottom {
        return;
    }
    screen.rule(x - 1, at, w + 2, Style::Frame);
    at += 1;
    if panel.rows.is_empty() {
        return;
    }
    screen.text_cut(x + 1, at, &crate::blocked::header(), width, Style::WindowDim);
    at += 1;
    let fits = bottom.saturating_sub(at);
    let end = panel.rows.len().saturating_sub(panel.scroll);
    let first = end.saturating_sub(fits);
    for row in &panel.rows[first..end] {
        screen.text_cut(x + 1, at, row, width, Style::Window);
        at += 1;
    }
}

/// `All` and each service, with the choice highlighted. The row slides along
/// when there are more than fit, so the choice is always on screen.
fn service_bar(panel: &LogsPanel, screen: &mut Screen, x: usize, y: usize, w: usize) {
    const LONGEST: usize = 20;
    let names: Vec<String> = std::iter::once("All".to_owned())
        .chain(panel.services.iter().map(|name| cut(name, LONGEST)))
        .collect();
    let chosen = panel.chosen_index();
    let widths: Vec<usize> = names.iter().map(|name| name.chars().count() + 2).collect();
    let mut start = 0;
    while start < chosen && widths[start..=chosen].iter().sum::<usize>() > w {
        start += 1;
    }
    let mut at = x;
    for (index, name) in names.iter().enumerate().skip(start) {
        if at + widths[index] > x + w {
            break;
        }
        let style = if index == chosen {
            Style::Selected
        } else {
            Style::Window
        };
        screen.text(at, y, &format!(" {name} "), style);
        at += widths[index];
    }
}

fn draw_dialog(screen: &mut Screen, dialog: &Dialog) {
    let width = (screen.width() * 3 / 4).clamp(40, 76);
    let height = (dialog.lines.len() + 6).min(screen.height().saturating_sub(4));
    let x = (screen.width().saturating_sub(width)) / 2;
    let y = (screen.height().saturating_sub(height)) / 2;
    let box_ = Rect::new(x, y, width, height);

    screen.shadow(box_);
    screen.fill(x, y, width, height, ' ', Style::Dialog);
    screen.frame(box_, Style::DialogFrame, &dialog.title, Style::Dialog);
    for (offset, text) in dialog.lines.iter().enumerate() {
        if y + 2 + offset >= y + height - 3 {
            break;
        }
        screen.text_cut(x + 2, y + 2 + offset, text, width - 4, Style::Dialog);
    }

    // Two buttons, the agreeing one marked as the one that acts.
    let buttons_y = y + height - 2;
    let yes = "  Yes  ";
    let no = "  No  ";
    let total = yes.len() + no.len() + 4;
    let start = x + (width.saturating_sub(total)) / 2;
    screen.text(start, buttons_y, yes, Style::Button);
    screen.text(start + 2, buttons_y, "Y", Style::ButtonHot);
    screen.text(start + yes.len() + 4, buttons_y, no, Style::Dialog);
    screen.text(start + yes.len() + 6, buttons_y, "N", Style::DialogHot);
}

/// A key the interface understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Open(Pane),
    Up,
    Down,
    Tab,
    Quit,
    Refresh,
    Start,
    Stop,
    Destroy,
    Help,
    PreviousService,
    NextService,
    PageUp,
    PageDown,
    /// Back to the newest line, which resumes following a log.
    Follow,
    /// A shell in the selected project's sandbox.
    Shell,
    Yes,
    No,
    Other,
}

/// Reads one keystroke. Function keys work as the bar along the bottom says,
/// and the letters work too, because not every terminal sends every one.
pub fn key(bytes: &[u8]) -> Key {
    match bytes {
        b"\x1b[A" | b"\x1bOA" | b"k" => Key::Up,
        b"\x1b[B" | b"\x1bOB" | b"j" => Key::Down,
        b"\x1b[5~" => Key::PageUp,
        b"\x1b[6~" => Key::PageDown,
        b"f" | b"F" | b"\x1b[F" | b"\x1bOF" | b"\x1b[4~" | b"\x1b[8~" => Key::Follow,
        b"\t" | b"6" | b"\x1bOU" | b"\x1b[17~" => Key::Tab,
        b"q" | b"0" | b"\x03" | b"\x1b[21~" => Key::Quit,
        // The red letter in the menu always opens that panel, so a key someone
        // presses to look at something can never also stop a sandbox.
        b"p" | b"P" => Key::Open(Pane::Projects),
        b"t" | b"T" => Key::Open(Pane::Traffic),
        b"s" | b"S" => Key::Open(Pane::Secrets),
        b"r" | b"R" => Key::Open(Pane::Review),
        b"l" | b"L" => Key::Open(Pane::Logs),
        // The numbers in the bar along the bottom, and the function keys.
        b"5" | b"\x1b[15~" => Key::Refresh,
        b"2" | b"u" | b"\x1bOQ" => Key::Start,
        b"3" | b"\x1bOR" => Key::Stop,
        b"x" => Key::Destroy,
        b"e" | b"E" => Key::Shell,
        b"[" | b"\x1b[D" | b"\x1bOD" => Key::PreviousService,
        b"]" | b"\x1b[C" | b"\x1bOC" => Key::NextService,
        b"?" | b"1" | b"\x1bOP" => Key::Help,
        b"y" | b"Y" => Key::Yes,
        b"n" | b"N" | b"\x1b" => Key::No,
        _ => Key::Other,
    }
}

/// How many bytes the first key takes. A held key can arrive as several in one
/// read, and each one has to count.
pub fn first_key_len(bytes: &[u8]) -> usize {
    match bytes {
        [0x1b, b'[', rest @ ..] => rest
            .iter()
            .position(|byte| (0x40..=0x7e).contains(byte))
            .map_or(bytes.len(), |end| end + 3),
        [0x1b, b'O', _, ..] => 3,
        [] => 0,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_that_arrive_together_are_taken_one_at_a_time() {
        assert_eq!(first_key_len(b"\x1b[A\x1b[A"), 3);
        assert_eq!(first_key_len(b"\x1b[5~j"), 4);
        assert_eq!(first_key_len(b"\x1bOAj"), 3);
        assert_eq!(first_key_len(b"jk"), 1);
        assert_eq!(first_key_len(b"\x1b"), 1);
    }

    #[test]
    fn the_side_arrows_choose_a_service_and_the_page_keys_are_known() {
        assert_eq!(key(b"\x1b[D"), Key::PreviousService);
        assert_eq!(key(b"\x1b[C"), Key::NextService);
        assert_eq!(key(b"\x1b[5~"), Key::PageUp);
        assert_eq!(key(b"\x1b[6~"), Key::PageDown);
    }

    #[test]
    fn the_logs_panel_scrolls_back_and_stops_at_each_end() {
        let mut m = logs_model();
        m.logs.lines = (0..500).map(|n| format!("line-{n:03}")).collect();
        m.logs.scroll_by(100);
        let text = plain(&m, 80, 24);
        assert!(text.contains("line-399"), "{text}");
        assert!(!text.contains("line-400"), "{text}");
        m.logs.scroll_by(100_000);
        assert_eq!(m.logs.scroll, 499);
        assert!(plain(&m, 80, 24).contains("line-000"));
        m.logs.scroll_by(-100_000);
        assert_eq!(m.logs.scroll, 0);
        m.logs.scroll_by(5);
        m.logs.choose(1);
        assert_eq!(m.logs.scroll, 0);
    }

    fn model() -> Model {
        Model {
            projects: vec![
                Row {
                    name: "demo-shop-ab12cd34".to_owned(),
                    path: "/p/demo_shop".to_owned(),
                    state: RowState::Running,
                },
                Row {
                    name: "notes-ff00ff00".to_owned(),
                    path: "/p/notes".to_owned(),
                    state: RowState::Stopped,
                },
            ],
            ..Model::default()
        }
    }

    fn plain(model: &Model, width: usize, height: usize) -> String {
        compose(model, width, height).plain()
    }

    #[test]
    fn the_screen_is_exactly_the_size_it_was_given() {
        for (width, height) in [(80, 24), (60, 14), (200, 60), (10, 2)] {
            let rows = render(&model(), width, height);
            assert_eq!(rows.len(), height.max(MIN_HEIGHT), "{width}x{height}");
            for row in &rows {
                let drawn: usize = row.iter().map(|s| s.text.chars().count()).sum();
                assert_eq!(drawn, width.max(MIN_WIDTH), "{width}x{height}");
            }
        }
    }

    /// The parts that make it look like a program from the time, rather than
    /// a modern one: a menu bar, a patterned desktop, a double-ruled window
    /// with its title set into the rule, a scrollbar, and numbered keys.
    #[test]
    fn the_frame_is_drawn_the_way_the_period_drew_it() {
        let text = plain(&model(), 90, 26);
        assert!(text.contains('▒'), "no patterned desktop");
        assert!(text.contains('╔') && text.contains('╝'), "no double rule");
        assert!(text.contains("╡ Projects ╞"), "no title in the rule");
        assert!(text.contains('▲') && text.contains('▼'), "no scrollbar");
        let bottom = text.lines().last().unwrap();
        for (number, word) in KEYS {
            assert!(bottom.contains(&format!("{number}{word}")), "{bottom}");
        }
        let top = text.lines().next().unwrap();
        for pane in Pane::ALL {
            assert!(top.contains(pane.label()), "{top}");
        }
    }

    #[test]
    fn the_window_casts_a_shadow() {
        let rows = render(&model(), 80, 24);
        let shadows = rows
            .iter()
            .filter(|row| row.iter().any(|s| s.style == Style::Shadow))
            .count();
        assert!(shadows > 2, "only {shadows} rows of shadow");
    }

    #[test]
    fn the_selected_row_is_highlighted_rather_than_marked_with_a_character() {
        let rows = render(&model(), 80, 24);
        let selected: usize = rows
            .iter()
            .filter(|row| row.iter().any(|s| s.style == Style::Selected))
            .count();
        assert_eq!(selected, 1, "exactly one row should be highlighted");
    }

    #[test]
    fn the_active_panel_is_marked_in_the_menu_bar() {
        let rows = render(&model(), 80, 24);
        assert!(rows[0].iter().any(|s| s.style == Style::MenuActive));
        assert!(rows[0].iter().any(|s| s.style == Style::MenuHot));
    }

    /// A name from a project could otherwise push the frame off the screen.
    #[test]
    fn a_very_long_name_cannot_break_the_frame() {
        let mut m = model();
        m.projects[0].name = "n".repeat(500);
        let text = plain(&m, 80, 24);
        for line in text.lines() {
            assert_eq!(line.chars().count(), 80);
        }
        assert!(text.contains('║'), "the frame survived");
    }

    #[test]
    fn the_warning_shows_until_something_else_needs_the_line() {
        assert!(plain(&model(), 80, 24).contains("Do not rely on it"));
        let mut m = model();
        // Words that cannot collide with a project's own state, which is
        // also drawn on screen.
        m.message = Some("finished tidying".to_owned());
        assert!(plain(&m, 80, 24).contains("finished tidying"));
        m.busy = Some("working away".to_owned());
        let text = plain(&m, 80, 24);
        assert!(text.contains("working away"), "{text}");
        assert!(!text.contains("finished tidying"), "{text}");
    }

    #[test]
    fn every_panel_that_is_not_built_says_so_rather_than_looking_empty() {
        for (pane, phrase) in [
            (Pane::Secrets, "delivered on each start"),
            (Pane::Review, "is not built"),
        ] {
            let mut m = model();
            m.pane = PaneState(pane);
            let text = plain(&m, 80, 24);
            assert!(text.contains(phrase), "{}: {text}", pane.label());
        }
    }

    fn logs_model() -> Model {
        let mut m = model();
        m.pane = PaneState(Pane::Logs);
        m.logs.services = vec!["api".to_owned(), "web".to_owned()];
        m
    }

    #[test]
    fn the_logs_panel_shows_its_services_and_lines_and_no_longer_says_not_built() {
        let mut m = logs_model();
        m.logs.lines = vec!["api-1  | hello there".to_owned()];
        let text = plain(&m, 80, 24);
        assert!(!text.contains("is not built"), "{text}");
        assert!(text.contains(" All  api  web "), "{text}");
        assert!(text.contains("api-1  | hello there"), "{text}");
        assert!(text.contains("<- -> service"), "{text}");
        assert!(text.contains("Output can be wrong"), "{text}");
        assert!(text.contains("╡ Logs ╞"), "{text}");
    }

    #[test]
    fn the_logs_panel_keeps_the_newest_lines_when_they_do_not_all_fit() {
        let mut m = logs_model();
        m.logs.lines = (0..500).map(|n| format!("line-{n:03}")).collect();
        let text = plain(&m, 80, 24);
        assert!(text.contains("line-499"), "{text}");
        assert!(!text.contains("line-000"), "{text}");
    }

    #[test]
    fn a_followed_log_says_so_and_a_paused_one_says_how_to_resume() {
        let mut m = logs_model();
        m.logs.lines = (0..100).map(|n| format!("line-{n:03}")).collect();
        m.logs.live = Live::Following;
        let text = plain(&m, 80, 24);
        assert!(text.contains("Following"), "{text}");
        assert!(!text.contains("Paused"), "{text}");

        m.logs.scroll_by(10);
        m.logs.push_line("line-100".to_owned());
        m.logs.push_line("line-101".to_owned());
        let text = plain(&m, 80, 24);
        assert!(text.contains("Paused, 2 new. f follows"), "{text}");
        assert!(!text.contains("line-101"), "the view moved: {text}");
        assert!(text.contains("line-089"), "{text}");

        m.logs.jump_to_newest();
        let text = plain(&m, 80, 24);
        assert!(text.contains("line-101") && text.contains("Following"), "{text}");

        m.logs.live = Live::Ended;
        assert!(plain(&m, 80, 24).contains("Not following"));
    }

    /// The tag must survive the smallest screen and a project name that would
    /// otherwise push it off.
    #[test]
    fn the_paused_tag_fits_beside_a_long_name_on_a_small_screen() {
        let mut m = logs_model();
        m.projects[0].name = "n".repeat(300);
        m.logs.lines = (0..100).map(|n| format!("line-{n:03}")).collect();
        m.logs.live = Live::Following;
        m.logs.scroll_by(5);
        let text = plain(&m, MIN_WIDTH, MIN_HEIGHT);
        assert!(text.contains("Paused"), "{text}");
        for line in text.lines() {
            assert_eq!(line.chars().count(), MIN_WIDTH);
        }
    }

    #[test]
    fn trimming_old_lines_keeps_the_view_counted_from_the_newest() {
        let mut panel = LogsPanel::default();
        for n in 0..50 {
            panel.push_line(format!("{n}"));
        }
        panel.scroll_by(5);
        panel.trim(20);
        assert_eq!(panel.lines.len(), 20);
        assert_eq!(panel.lines[0], "30");
        assert_eq!(panel.scroll, 5);
        panel.scroll_by(100);
        panel.trim(10);
        assert_eq!(panel.scroll, 9);
    }

    #[test]
    fn f_and_end_return_to_the_newest_line() {
        for bytes in [&b"f"[..], b"\x1b[F", b"\x1bOF", b"\x1b[4~"] {
            assert_eq!(key(bytes), Key::Follow, "{bytes:?}");
        }
    }

    #[test]
    fn a_long_log_line_cannot_break_the_frame() {
        let mut m = logs_model();
        m.logs.lines = vec!["x".repeat(5000)];
        for line in plain(&m, 80, 24).lines() {
            assert_eq!(line.chars().count(), 80);
        }
    }

    #[test]
    fn the_choice_moves_along_all_and_the_services_and_stops_at_each_end() {
        let mut panel = logs_model().logs;
        assert_eq!(panel.chosen, None);
        panel.choose(-1);
        assert_eq!(panel.chosen, None);
        panel.choose(1);
        assert_eq!(panel.chosen.as_deref(), Some("api"));
        panel.choose(1);
        assert_eq!(panel.chosen.as_deref(), Some("web"));
        panel.choose(1);
        assert_eq!(panel.chosen.as_deref(), Some("web"));
        panel.choose(-1);
        panel.choose(-1);
        assert_eq!(panel.chosen, None);
        let mut empty = LogsPanel::default();
        empty.choose(1);
        assert_eq!(empty.chosen, None);
    }

    #[test]
    fn another_project_starts_with_an_empty_panel() {
        let mut panel = logs_model().logs;
        panel.project = "/p/one".to_owned();
        panel.lines = vec!["from one".to_owned()];
        panel.chosen = Some("api".to_owned());
        panel.belong_to("/p/one");
        assert_eq!(panel.lines, ["from one"]);
        panel.belong_to("/p/two");
        assert_eq!(panel.project, "/p/two");
        assert!(panel.lines.is_empty());
        assert!(panel.services.is_empty());
        assert_eq!(panel.chosen, None);
    }

    #[test]
    fn the_chosen_service_is_highlighted_even_far_along_a_long_row() {
        let mut m = logs_model();
        m.logs.services = (0..40).map(|n| format!("service-{n:02}")).collect();
        m.logs.chosen = Some("service-39".to_owned());
        let rows = render(&m, 80, 24);
        let highlighted: String = rows
            .iter()
            .flatten()
            .filter(|span| span.style == Style::Selected)
            .map(|span| span.text.as_str())
            .collect();
        assert!(highlighted.contains("service-39"), "{highlighted}");
    }

    /// The keys that choose a service must not do anything else, and none of
    /// the keys that act on a sandbox may have moved onto them.
    #[test]
    fn choosing_a_service_uses_its_own_keys() {
        assert_eq!(key(b"["), Key::PreviousService);
        assert_eq!(key(b"]"), Key::NextService);
        assert_eq!(key(b"3"), Key::Stop);
        assert_eq!(key(b"x"), Key::Destroy);
        assert_eq!(key(b"l"), Key::Open(Pane::Logs));
        assert_eq!(key(b"5"), Key::Refresh);
    }

    fn traffic_model() -> Model {
        let mut m = model();
        m.pane = PaneState(Pane::Traffic);
        m
    }

    /// The words that matter, whatever was or was not read, and however small
    /// the screen: they are broken across rows, never cut off.
    #[test]
    fn the_traffic_panel_always_says_what_it_cannot_show() {
        for (width, height) in [(80, 24), (60, 14), (200, 60)] {
            let mut m = traffic_model();
            let text = flat(&m, width, height);
            for phrase in ["hostnames only", "Raw IP, metadata and UDP", "doesn't mean nothing was tried"] {
                assert!(text.contains(phrase), "{width}x{height}: {phrase}: {text}");
            }
            m.traffic.rows = (0..500).map(|n| format!("row-{n:03}")).collect();
            m.traffic.scroll_by(200);
            let text = flat(&m, width, height);
            assert!(text.contains("doesn't mean nothing was tried"), "{width}x{height}");
        }
    }

    /// The screen's text with its frame and line breaks taken out, so a phrase
    /// broken across rows can still be found.
    fn flat(model: &Model, width: usize, height: usize) -> String {
        plain(model, width, height)
            .lines()
            .map(|line| line.replace(['║', '▒', '░', '█'], " "))
            .map(|line| line.trim().to_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn a_sandbox_that_records_nothing_says_so_even_on_the_smallest_screen() {
        let mut m = traffic_model();
        m.traffic.notes = vec![Line::styled(
            crate::blocked::not_recording_note(),
            Style::WindowAlert,
        )];
        for (width, height) in [(80, 24), (60, 14)] {
            let text = flat(&m, width, height);
            assert!(text.contains("Nothing is being recorded"), "{text}");
        }
        let text = flat(&m, 80, 24);
        assert!(text.contains("down --destroy"), "{text}");
        assert!(text.contains("deletes its data"), "{text}");
    }

    #[test]
    fn the_traffic_panel_keeps_the_newest_rows_and_scrolls_back_and_stops_at_each_end() {
        let mut m = traffic_model();
        m.traffic.rows = (0..500).map(|n| format!("row-{n:03}")).collect();
        let text = plain(&m, 100, 24);
        assert!(text.contains("row-499"), "{text}");
        assert!(!text.contains("row-000"), "{text}");
        assert!(text.contains("HOSTNAME"), "{text}");
        m.traffic.scroll_by(100);
        let text = plain(&m, 100, 24);
        assert!(text.contains("row-399"), "{text}");
        assert!(!text.contains("row-400"), "{text}");
        m.traffic.scroll_by(100_000);
        assert_eq!(m.traffic.scroll, 499);
        assert!(plain(&m, 100, 24).contains("row-000"));
        m.traffic.scroll_by(-100_000);
        assert_eq!(m.traffic.scroll, 0);
    }

    #[test]
    fn a_long_hostname_cannot_break_the_frame() {
        let mut m = traffic_model();
        m.traffic.rows = vec!["x".repeat(5000)];
        for line in plain(&m, 80, 24).lines() {
            assert_eq!(line.chars().count(), 80);
        }
    }

    #[test]
    fn another_project_starts_with_an_empty_traffic_panel() {
        let mut panel = TrafficPanel {
            project: "/p/one".to_owned(),
            rows: vec!["from one".to_owned()],
            ..TrafficPanel::default()
        };
        panel.belong_to("/p/one");
        assert_eq!(panel.rows, ["from one"]);
        panel.belong_to("/p/two");
        assert!(panel.rows.is_empty());
    }

    #[test]
    fn wrapping_breaks_at_spaces_and_keeps_every_word() {
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap("", 7), Vec::<String>::new());
        assert_eq!(wrap("longerthanwidth x", 4), ["longerthanwidth", "x"]);
    }

    #[test]
    fn a_question_is_a_box_over_the_rest_with_its_own_shadow() {
        let mut m = model();
        m.dialog = Some(Dialog {
            title: "Start demo-shop".to_owned(),
            lines: vec!["Forward 3000 and 8080 to 127.0.0.1?".to_owned()],
        });
        let text = plain(&m, 90, 26);
        assert!(text.contains("╡ Start demo-shop ╞"), "{text}");
        assert!(text.contains("Yes") && text.contains("No"));
        let rows = render(&m, 90, 26);
        assert!(rows.iter().any(|r| r.iter().any(|s| s.style == Style::Button)));
    }

    #[test]
    fn selection_stays_inside_the_list() {
        let mut m = model();
        m.move_by(-1);
        assert_eq!(m.selected, 0);
        m.move_by(1);
        assert_eq!(m.selected, 1);
        m.move_by(50);
        assert_eq!(m.selected, 1);
        let mut empty = Model::default();
        empty.move_by(1);
        assert_eq!(empty.selected, 0);
        assert!(empty.selected_row().is_none());
    }

    #[test]
    fn panels_cycle() {
        let mut pane = Pane::Projects;
        for _ in 0..Pane::ALL.len() {
            pane = pane.next();
        }
        assert_eq!(pane, Pane::Projects);
    }

    #[test]
    fn the_function_keys_match_the_bar_and_the_letters_still_work() {
        assert_eq!(key(b"\x1bOQ"), Key::Start);
        assert_eq!(key(b"u"), Key::Start);
        assert_eq!(key(b"\x1bOR"), Key::Stop);
        assert_eq!(key(b"3"), Key::Stop);
        // The menu's red letter opens its panel and never stops a sandbox.
        assert_eq!(key(b"s"), Key::Open(Pane::Secrets));
        assert_eq!(key(b"r"), Key::Open(Pane::Review));
        assert_eq!(key(b"\x1b[15~"), Key::Refresh);
        assert_eq!(key(b"\x1b[21~"), Key::Quit);
        assert_eq!(key(b"q"), Key::Quit);
        // Raw mode stops the shell ending this, so the interface has to.
        assert_eq!(key(b"\x03"), Key::Quit);
        assert_eq!(key(b"\x1b[A"), Key::Up);
        assert_eq!(key(b"y"), Key::Yes);
        assert_eq!(key(b"n"), Key::No);
        assert_eq!(key(b"\x1b"), Key::No);
        assert_eq!(key(b"e"), Key::Shell);
        assert_eq!(key(b"Z"), Key::Other);
    }
}
