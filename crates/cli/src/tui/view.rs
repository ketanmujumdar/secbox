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

    /// A mark for the list, where a word would take the room a name needs. A
    /// tape deck's play and stop, which needs no legend.
    fn mark(self) -> char {
        match self {
            RowState::Running => '►',
            RowState::Stopped => '■',
            RowState::None => '·',
            RowState::Unknown => '?',
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

/// One line in a box on the Projects panel: a name on the left and, if there
/// is one, a value set to the right in `style`. With no value the whole line
/// takes `style`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub left: String,
    pub right: String,
    pub style: Style,
}

impl Entry {
    pub fn new(left: impl Into<String>, right: impl Into<String>, style: Style) -> Self {
        Self {
            left: left.into(),
            right: right.into(),
            style,
        }
    }
}

/// What the Projects panel shows about the selected project, from its status
/// report. Every string is already escaped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overview {
    pub path: String,
    /// Why there is nothing else to show, such as a daemon that did not answer.
    pub problem: Option<String>,
    pub services: Vec<Entry>,
    pub ports: Vec<Entry>,
    pub resources: Vec<Entry>,
    pub blocked: Vec<Entry>,
    /// Whether the Blocked box is drawn in alert colour.
    pub blocked_alert: bool,
}

/// The steps of a start so far, shown in a box while it runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    pub title: String,
    /// Oldest first, escaped and cut, and only the most recent kept.
    pub steps: Vec<String>,
}

impl Progress {
    /// The most steps kept. Only the last few are ever on screen.
    const KEPT: usize = 200;
    /// The longest a step is kept, before the box cuts it to its width.
    const LONGEST: usize = 300;

    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            steps: Vec::new(),
        }
    }

    /// Adds a step as the daemon reported it. A step can name things from
    /// the project or the sandbox, so it is escaped and cut here, and a line
    /// break inside one becomes a space rather than a broken box.
    pub fn push(&mut self, step: &str, detail: Option<&str>) {
        let text = match detail {
            Some(detail) => format!("{step}: {detail}"),
            None => step.to_owned(),
        };
        let flat = text.replace(['\n', '\r'], " ");
        self.steps
            .push(cut(&proto::text::escape(&flat), Self::LONGEST));
        if self.steps.len() > Self::KEPT {
            self.steps.remove(0);
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

/// One secret on the Secrets panel. Never its value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretRow {
    pub name: String,
    /// How it was stored: typed at a prompt, or imported from a file.
    pub source: String,
    /// The services it is assigned to, and how it reaches them.
    pub goes_to: String,
}

/// What the Secrets panel shows for the selected project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretsPanel {
    pub rows: Vec<SecretRow>,
    /// Why there are no rows, when there are none.
    pub note: Option<Line>,
    /// Where the secrets could be sent, from the daemon. Already escaped.
    pub reach: Vec<String>,
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
    /// What stops the selected project starting, shown in place of its boxes.
    pub detail: Vec<Line>,
    pub overview: Option<Overview>,
    pub message: Option<String>,
    pub busy: Option<String>,
    pub pane: PaneState,
    pub dialog: Option<Dialog>,
    pub progress: Option<Progress>,
    pub logs: LogsPanel,
    pub traffic: TrafficPanel,
    pub secrets: SecretsPanel,
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
        Pane::Projects | Pane::Logs | Pane::Traffic | Pane::Secrets => &[],
        Pane::Review => &[
            "Reviewing changes out of a sandbox is not built.",
            "",
            "The broker that quarantines a bundle, flags risky paths and pushes",
            "one approved commit exists, but no command reaches it yet.",
        ],
    }
}

/// One entry in the bar along the bottom: the key, then what it does.
pub type KeyLabel = (&'static str, &'static str);

/// The bar along the bottom, as every program of the period had: a key and a
/// word. It lists only what works on the panel showing, so that nothing on it
/// is a key that does nothing, and the keys that act on a sandbox appear only
/// where they work, on Projects.
pub fn keys_for(pane: Pane) -> &'static [KeyLabel] {
    match pane {
        Pane::Projects => &[
            ("1", "Help"),
            ("2", "Start"),
            ("3", "Stop"),
            ("x", "Destroy"),
            ("e", "Shell"),
            ("5", "Refresh"),
            ("6", "Panel"),
            ("10", "Quit"),
        ],
        Pane::Logs => &[
            ("1", "Help"),
            ("←→", "Service"),
            ("↑↓", "Scroll"),
            ("f", "Follow"),
            ("e", "Shell"),
            ("5", "Refresh"),
            ("6", "Panel"),
            ("10", "Quit"),
        ],
        Pane::Traffic => &[
            ("1", "Help"),
            ("↑↓", "Scroll"),
            ("5", "Refresh"),
            ("6", "Panel"),
            ("10", "Quit"),
        ],
        Pane::Secrets => &[
            ("1", "Help"),
            ("↑↓", "Project"),
            ("5", "Refresh"),
            ("6", "Panel"),
            ("10", "Quit"),
        ],
        Pane::Review => &[("1", "Help"), ("6", "Panel"), ("10", "Quit")],
    }
}

/// What `?` says on each panel. It names the same keys as the bar, in words.
pub fn help_for(pane: Pane) -> &'static str {
    match pane {
        Pane::Projects => "Up and down choose. 2 starts, 3 stops, x destroys, e opens a shell, 5 reads again. Red letters open panels, tab moves on, q quits.",
        Pane::Logs => "New lines follow by themselves. Left and right choose a service or All. Up and PgUp scroll back and pause; f or End follows again. e opens a shell in the chosen service. 5 reads again.",
        Pane::Traffic => "Up, down, PgUp and PgDn scroll. 5 reads the newest.",
        Pane::Secrets => "Names and where they go, never values. Up and down choose the project, 5 reads again. Change them with the secrets command.",
        Pane::Review => "Reviewing changes is not built yet. Red letters open panels, tab moves on, q quits.",
    }
}

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
    status_bar(&mut screen, model.pane.0);

    let window = Rect::new(1, 1, width - 4, height - 4);
    screen.shadow(window);
    screen.fill(window.x, window.y, window.w, window.h, ' ', Style::Window);
    screen.frame(window, Style::Frame, model.pane.0.label(), Style::Title);

    let inner_x = window.x + 1;
    let inner_y = window.y + 1;
    let inner_w = window.w - 2;
    let inner_h = window.h - 2;
    body(model, &mut screen, inner_x, inner_y, inner_w, inner_h);

    // The last line inside the window is for what just happened, or what is
    // happening now. The standing warning has its own place in the menu bar.
    let note_y = inner_y + inner_h - 1;
    let note = model.busy.as_ref().or(model.message.as_ref());
    if let Some(note) = note {
        screen.fill(inner_x, note_y, inner_w, 1, ' ', Style::WindowWarn);
        screen.text_cut(inner_x + 1, note_y, note, inner_w - 2, Style::WindowWarn);
    }

    if let Some(progress) = &model.progress {
        draw_progress(&mut screen, progress);
    }
    if let Some(dialog) = &model.dialog {
        draw_dialog(&mut screen, dialog);
    }
    screen
}

/// That this is an early build, longest first. It sits at the right of the
/// menu bar, where nothing else needs the room, so that it is on screen at all
/// times and the line at the bottom of the window is free for messages.
const WARNINGS: [&str; 3] = [
    "Early build. Do not rely on it for untrusted code.",
    "Early build. Do not rely on it.",
    "Early build",
];

fn menu_bar(screen: &mut Screen, active: Pane) {
    screen.fill(0, 0, screen.width(), 1, ' ', Style::MenuBar);
    let menu = |gap: usize| -> usize {
        1 + Pane::ALL
            .iter()
            .map(|pane| pane.label().chars().count() + gap)
            .sum::<usize>()
    };
    // The entries close up before the warning is shortened, and the
    // shortest warning always has room, even on the narrowest screen.
    let fits = |text: &str, gap: usize| menu(gap) + text.chars().count() + 3 <= screen.width();
    let (warning, gap) = WARNINGS
        .iter()
        .flat_map(|text| [(*text, 3), (*text, 2)])
        .find(|(text, gap)| fits(text, *gap))
        .unwrap_or((WARNINGS[WARNINGS.len() - 1], 2));
    let tag = format!(" {warning} ");
    let tag_x = screen.width().saturating_sub(tag.chars().count() + 1);
    screen.text(tag_x, 0, &tag, Style::MenuAlert);

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
        x += label.chars().count() + gap;
    }
}

/// Draws the panel's keys. When they do not all fit, the gaps close up first,
/// then the least needed go, from the middle of the list: help and quitting
/// stay at the two ends, where they always were.
fn status_bar(screen: &mut Screen, pane: Pane) {
    let y = screen.height() - 1;
    screen.fill(0, y, screen.width(), 1, ' ', Style::StatusLabel);
    let mut keys: Vec<KeyLabel> = keys_for(pane).to_vec();
    let room = screen.width().saturating_sub(2);
    let needed = |keys: &[KeyLabel], gap: usize| -> usize {
        let text: usize = keys
            .iter()
            .map(|(key, word)| key.chars().count() + word.chars().count())
            .sum();
        text + gap * keys.len().saturating_sub(1)
    };
    let mut gap = 2;
    if needed(&keys, gap) > room {
        gap = 1;
    }
    while needed(&keys, gap) > room && keys.len() > 2 {
        // Just before Quit is the least needed, so it goes first.
        keys.remove(keys.len() - 2);
    }
    let mut x = 1;
    for (key, word) in keys {
        x += screen.text(x, y, key, Style::StatusKey);
        x += screen.text(x, y, word, Style::StatusLabel);
        x += gap;
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
    if model.pane.0 == Pane::Secrets {
        secrets_body(model, screen, x, y, w, room);
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

    // A narrow list down the left, and the selected project on the right.
    let list_w = (w / 4).clamp(20, 30);
    project_list(model, screen, Rect::new(x, y, list_w, room));
    for row in y..y + room {
        screen.put(x + list_w, row, '│', Style::Frame);
    }
    let right = Rect::new(x + list_w + 2, y, w - list_w - 3, room);
    project_detail(model, screen, right);
}

/// One row per project: a mark for its state and its name. The mark is in the
/// state's colour, and the selected row is highlighted rather than marked.
fn project_list(model: &Model, screen: &mut Screen, at: Rect) {
    // At least three rows, so the list always has a scrollbar to sit beside.
    let list_h = at.h.max(3);
    let first = model.selected.saturating_sub(list_h.saturating_sub(1));
    let line_w = at.w - 1;
    for offset in 0..list_h.min(at.h) {
        let Some(row) = model.projects.get(first + offset) else {
            break;
        };
        let selected = first + offset == model.selected;
        let line_y = at.y + offset;
        if selected {
            screen.fill(at.x, line_y, line_w, 1, ' ', Style::Selected);
        }
        let (name_style, mark_style) = if selected {
            (Style::Selected, Style::Selected)
        } else {
            (Style::Window, row.state.style())
        };
        screen.put(at.x + 1, line_y, row.state.mark(), mark_style);
        screen.text_cut(at.x + 3, line_y, &row.name, line_w.saturating_sub(4), name_style);
    }
    screen.scrollbar(at.x + at.w - 1, at.y, at.h, first, at.h, model.projects.len());
}

/// The selected project's name and state, then its services, ports,
/// resources and blocked lookups in boxes. On a small screen the boxes stack,
/// and the ones that matter least go first.
fn project_detail(model: &Model, screen: &mut Screen, at: Rect) {
    let Some(row) = model.selected_row() else {
        return;
    };
    let bottom = at.y + at.h;
    let word = row.state.word();
    let name_room = at.w.saturating_sub(word.chars().count() + 2);
    let name = cut(&row.name, name_room);
    screen.text(at.x, at.y, &name, Style::Title);
    screen.text(at.x + name.chars().count() + 2, at.y, word, row.state.style());
    let mut y = at.y + 1;

    // A plan that cannot start says why, in place of everything else.
    if !model.detail.is_empty() {
        for line in &model.detail {
            for piece in wrap(&line.text, at.w) {
                if y < bottom {
                    screen.text_cut(at.x, y, &piece, at.w, line.style);
                    y += 1;
                }
            }
        }
        return;
    }
    let Some(overview) = &model.overview else {
        return;
    };
    if let Some(problem) = &overview.problem {
        for piece in wrap(problem, at.w) {
            if y < bottom {
                screen.text_cut(at.x, y, &piece, at.w, Style::WindowAlert);
                y += 1;
            }
        }
        return;
    }

    // What the sandbox said about itself is marked as such, just under its
    // boxes, and broken across rows rather than cut.
    let caveat = if overview.services.is_empty() {
        Vec::new()
    } else {
        wrap("these came from inside the sandbox, so they can be wrong", at.w)
    };
    let boxes_bottom = bottom.saturating_sub(caveat.len());
    // The path is the first thing to go when room is short.
    if boxes_bottom.saturating_sub(y) >= 8 {
        screen.text_cut(at.x, y, &overview.path, at.w, Style::WindowDim);
        y += 1;
    }
    let area = Rect::new(at.x, y, at.w, boxes_bottom.saturating_sub(y));
    if let Some(under) = draw_groups(screen, area, overview) {
        for (offset, piece) in caveat.iter().enumerate() {
            screen.text_cut(at.x, under + offset, piece, at.w, Style::WindowWarn);
        }
    }
}

/// One framed box's worth of the overview.
struct Group<'a> {
    title: &'static str,
    entries: &'a [Entry],
    empty: &'static str,
    alert: bool,
}

impl Group<'_> {
    /// Rows it would like: its entries, or the line saying there are none,
    /// inside a frame.
    fn wanted(&self) -> usize {
        self.entries.len().max(1) + 2
    }
}

/// The smallest a box can be: a frame round one line.
const GROUP_MIN: usize = 3;

/// Lays the boxes out and draws them. Two columns when there is width for
/// them, one otherwise. Returns the row under the boxes if the services box
/// was among them, since what it says needs a caveat there.
fn draw_groups(screen: &mut Screen, area: Rect, overview: &Overview) -> Option<usize> {
    let services = Group {
        title: "Services",
        entries: &overview.services,
        empty: "None reported",
        alert: false,
    };
    let ports = Group {
        title: "Ports",
        entries: &overview.ports,
        empty: "None forwarded",
        alert: false,
    };
    let resources = Group {
        title: "Resources",
        entries: &overview.resources,
        empty: "Not reported",
        alert: false,
    };
    let blocked = Group {
        title: "Blocked",
        entries: &overview.blocked,
        empty: "Unknown",
        alert: overview.blocked_alert,
    };

    if area.w >= 50 && area.h >= 2 * GROUP_MIN {
        let half = (area.w - 1) / 2;
        let left_x = area.x;
        let right_x = area.x + half + 1;
        let right_w = area.w - half - 1;
        let top_want = services.wanted().max(resources.wanted());
        let bottom_want = ports.wanted().max(blocked.wanted());
        let bottom_h = bottom_want.min(area.h - GROUP_MIN).max(GROUP_MIN);
        let top_h = top_want.min(area.h - bottom_h).max(GROUP_MIN);
        let bottom_y = area.y + top_h;
        draw_group(screen, Rect::new(left_x, area.y, half, top_h), &services);
        draw_group(screen, Rect::new(right_x, area.y, right_w, top_h), &resources);
        draw_group(screen, Rect::new(left_x, bottom_y, half, bottom_h), &ports);
        draw_group(screen, Rect::new(right_x, bottom_y, right_w, bottom_h), &blocked);
        return Some(bottom_y + bottom_h);
    }

    // One column. Room goes to the boxes that matter most, the frame and one
    // line each first, then what more each would like, in the same order.
    let order = [&blocked, &services, &ports, &resources];
    let mut heights = [0usize; 4];
    let mut left = area.h;
    for (index, _) in order.iter().enumerate() {
        if left >= GROUP_MIN {
            heights[index] = GROUP_MIN;
            left -= GROUP_MIN;
        }
    }
    for (index, group) in order.iter().enumerate() {
        if heights[index] > 0 {
            let more = (group.wanted() - GROUP_MIN).min(left);
            heights[index] += more;
            left -= more;
        }
    }
    // Drawn top to bottom in the order a person reads them.
    let mut y = area.y;
    for index in [1, 0, 2, 3] {
        if heights[index] > 0 {
            draw_group(screen, Rect::new(area.x, y, area.w, heights[index]), order[index]);
            y += heights[index];
        }
    }
    (heights[1] > 0).then_some(y)
}

/// A single-ruled box with its title in the top rule, the way the period
/// grouped things inside a window. Each entry is a line, its second part set
/// to the right. When they do not all fit, the last line says how many more.
fn draw_group(screen: &mut Screen, at: Rect, group: &Group) {
    if at.w < 6 || at.h < GROUP_MIN {
        return;
    }
    let frame = if group.alert {
        Style::FrameAlert
    } else {
        Style::Frame
    };
    screen.group_box(at, frame, group.title, Style::Title);
    let inner_w = at.w - 4;
    let rows = at.h - 2;
    if group.entries.is_empty() {
        screen.text_cut(at.x + 2, at.y + 1, group.empty, inner_w, Style::WindowDim);
        return;
    }
    let shown = if group.entries.len() > rows {
        rows - 1
    } else {
        rows
    };
    for (offset, entry) in group.entries.iter().take(shown).enumerate() {
        let y = at.y + 1 + offset;
        let right = cut(&entry.right, inner_w / 2);
        let right_w = right.chars().count();
        let left_room = if right_w == 0 {
            inner_w
        } else {
            inner_w.saturating_sub(right_w + 1)
        };
        // With two parts only the value is coloured; with one, the whole line,
        // so a count in alert colour is set on red and cannot be missed.
        let left_style = if entry.right.is_empty() { entry.style } else { Style::Window };
        screen.text_cut(at.x + 2, y, &entry.left, left_room, left_style);
        screen.text(at.x + 2 + inner_w - right_w, y, &right, entry.style);
    }
    if shown < group.entries.len() {
        screen.text_cut(
            at.x + 2,
            at.y + 1 + shown,
            &format!("and {} more", group.entries.len() - shown),
            inner_w,
            Style::WindowDim,
        );
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
        "Output comes from inside the sandbox, so it can be wrong.",
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

/// The selected project's secrets: a name, how it was stored, and where it
/// goes, in columns, then what is always true about them. Read-only: changing
/// them stays with the secrets command, which asks for values at a hidden
/// prompt.
fn secrets_body(model: &Model, screen: &mut Screen, x: usize, y: usize, w: usize, room: usize) {
    let panel = &model.secrets;
    let width = w - 2;
    let bottom = y + room;
    let Some(row) = model.selected_row() else {
        screen.text_cut(x + 1, y, "No project has been started on this machine.", width, Style::Window);
        return;
    };
    screen.text_cut(x + 1, y, &row.name, width, Style::WindowDim);

    // What is always true goes at the bottom, and is kept however many
    // secrets there are, so the rows get what is left.
    let footer: Vec<(String, Style)> = [
        ("Names only. Values are never shown, and the keychain was not read.", Style::WindowWarn),
        ("A service can read every secret it is given.", Style::WindowDim),
    ]
    .iter()
    .flat_map(|(text, style)| wrap(text, width).into_iter().map(|piece| (piece, *style)))
    .chain(
        wrap(
            &format!("Add and assign them with: {} secrets set NAME --service SERVICE", brand::CLI_BIN),
            width,
        )
        .into_iter()
        .map(|piece| (piece, Style::WindowDim)),
    )
    .collect();
    let footer_y = bottom.saturating_sub(footer.len()).max(y + 1);
    for (offset, (text, style)) in footer.iter().enumerate() {
        if footer_y + offset < bottom {
            screen.text_cut(x + 1, footer_y + offset, text, width, *style);
        }
    }
    let rows_bottom = footer_y.saturating_sub(1);

    let mut at = y + 2;
    if let Some(note) = &panel.note {
        if at < rows_bottom {
            screen.text_cut(x + 1, at, &note.text, width, note.style);
        }
        return;
    }
    if panel.rows.is_empty() || at >= rows_bottom {
        return;
    }
    let name_w = panel
        .rows
        .iter()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(4, (width / 3).max(4));
    let source_w = 8;
    let columns = |name: &str, source: &str, goes: &str| {
        format!("{:<name_w$}  {:<source_w$}  {goes}", cut(name, name_w), source)
    };
    screen.text_cut(x + 1, at, &columns("NAME", "SOURCE", "GOES TO"), width, Style::WindowDim);
    at += 1;
    let fits = rows_bottom.saturating_sub(at);
    let shown = if panel.rows.len() > fits { fits.saturating_sub(1) } else { fits };
    for secret in panel.rows.iter().take(shown) {
        let style = if secret.goes_to == "no service" {
            Style::WindowDim
        } else {
            Style::Window
        };
        screen.text_cut(x + 1, at, &columns(&secret.name, &secret.source, &secret.goes_to), width, style);
        at += 1;
    }
    if shown < panel.rows.len() && at < rows_bottom {
        screen.text_cut(
            x + 1,
            at,
            &format!("and {} more. The secrets list command shows them all.", panel.rows.len() - shown),
            width,
            Style::WindowDim,
        );
        at += 1;
    }
    // Where they could go, in the room left, wrapped rather than cut so a
    // host name is never shown half missing.
    at += 1;
    for line in &panel.reach {
        for piece in wrap(line, width) {
            if at >= rows_bottom {
                return;
            }
            screen.text_cut(x + 1, at, &piece, width, Style::WindowWarn);
            at += 1;
        }
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

/// The steps of a start in a grey box over everything, the newest at the
/// bottom and marked, the ones before it ticked off. Older steps scroll away
/// at the top, as on a printer.
fn draw_progress(screen: &mut Screen, progress: &Progress) {
    let width = (screen.width() * 3 / 4).clamp(40, 76);
    let height = (progress.steps.len().max(1) + 4).clamp(6, screen.height().saturating_sub(4).max(6));
    let x = (screen.width().saturating_sub(width)) / 2;
    let y = (screen.height().saturating_sub(height)) / 2;
    let box_ = Rect::new(x, y, width, height);
    screen.shadow(box_);
    screen.fill(x, y, width, height, ' ', Style::Dialog);
    screen.frame(box_, Style::DialogFrame, &progress.title, Style::Dialog);

    let rows = height - 4;
    let first = progress.steps.len().saturating_sub(rows);
    if progress.steps.is_empty() {
        screen.text_cut(x + 2, y + 2, "Waiting for the first step.", width - 4, Style::Dialog);
    }
    for (offset, step) in progress.steps[first..].iter().enumerate() {
        let newest = first + offset + 1 == progress.steps.len();
        let (mark, style) = if newest {
            ('►', Style::DialogHot)
        } else {
            ('√', Style::Dialog)
        };
        screen.put(x + 2, y + 2 + offset, mark, style);
        screen.text_cut(x + 4, y + 2 + offset, step, width - 6, Style::Dialog);
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
        for (number, word) in keys_for(Pane::Projects) {
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

    /// The warning is in the menu bar on every panel and every size, whatever
    /// the line at the bottom of the window is saying.
    #[test]
    fn the_warning_is_always_in_the_menu_bar_and_messages_have_the_bottom_line() {
        for (width, height) in [(MIN_WIDTH, MIN_HEIGHT), (80, 24), (200, 60)] {
            for pane in Pane::ALL {
                let mut m = model();
                m.pane = PaneState(pane);
                m.message = Some("finished tidying".to_owned());
                let text = plain(&m, width, height);
                let top = text.lines().next().unwrap();
                assert!(top.contains("Early build"), "{width}: {top}");
                for label in Pane::ALL.map(Pane::label) {
                    assert!(top.contains(label), "{width}: {label}: {top}");
                }
                assert!(text.contains("finished tidying"), "{width}x{height}: {text}");
            }
        }
        let wide = plain(&model(), 120, 24);
        assert!(wide.lines().next().unwrap().contains("Do not rely on it for untrusted code."));
        let rows = render(&model(), 80, 24);
        assert!(rows[0].iter().any(|s| s.style == Style::MenuAlert && s.text.contains("Early build")));
    }

    #[test]
    fn the_bottom_line_shows_what_is_happening_over_what_happened() {
        let mut m = model();
        // Words that cannot collide with a project's own state, which is
        // also drawn on screen.
        m.message = Some("finished tidying".to_owned());
        assert!(plain(&m, 80, 24).contains("finished tidying"));
        m.busy = Some("working away".to_owned());
        let text = plain(&m, 80, 24);
        assert!(text.contains("working away"), "{text}");
        assert!(!text.contains("finished tidying"), "{text}");
        // With nothing to say, nothing is said: the warning is not repeated.
        let quiet = plain(&model(), 80, 24);
        assert_eq!(quiet.matches("Early build").count(), 1, "{quiet}");
    }

    #[test]
    fn the_panel_that_is_not_built_says_so_rather_than_looking_empty() {
        let mut m = model();
        m.pane = PaneState(Pane::Review);
        let text = plain(&m, 80, 24);
        assert!(text.contains("is not built"), "{text}");
    }

    fn secrets_model() -> Model {
        let mut m = model();
        m.pane = PaneState(Pane::Secrets);
        m.secrets.rows = vec![
            SecretRow {
                name: "API_KEY".to_owned(),
                source: "prompt".to_owned(),
                goes_to: "api, worker: injected into the environment".to_owned(),
            },
            SecretRow {
                name: "DB_PASSWORD".to_owned(),
                source: "import".to_owned(),
                goes_to: "no service".to_owned(),
            },
        ];
        m
    }

    #[test]
    fn the_secrets_panel_says_where_the_secrets_could_be_sent() {
        let mut model = Model {
            projects: vec![Row {
                name: "demo".into(),
                path: "/p/demo".into(),
                state: RowState::Running,
            }],
            pane: PaneState(Pane::Secrets),
            ..Model::default()
        };
        model.secrets.rows = vec![SecretRow {
            name: "DEMO_TOKEN".into(),
            source: "keychain".into(),
            goes_to: "api".into(),
        }];
        model.secrets.reach = proto::exposure_lines(
            &[proto::HeldSecret { name: "DEMO_TOKEN".into(), services: vec!["api".into()] }],
            Some(&["storage.googleapis.com".to_owned()]),
            true,
        );
        let text = compose(&model, 100, 30).plain();
        assert!(text.contains("DEMO_TOKEN is given to api"), "{text}");
        assert!(text.contains("storage.googleapis.com"), "{text}");
    }

    #[test]
    fn the_secrets_panel_lists_names_and_where_they_go_and_never_a_value() {
        let text = plain(&secrets_model(), 100, 30);
        assert!(!text.contains("is not built"), "{text}");
        for phrase in [
            "NAME",
            "GOES TO",
            "API_KEY",
            "api, worker: injected into the environment",
            "DB_PASSWORD",
            "no service",
            "Values are never shown",
            "secrets set NAME",
        ] {
            assert!(text.contains(phrase), "{phrase}: {text}");
        }
    }

    /// Whatever the number of secrets and the size of the screen, the words
    /// saying values are never shown stay, and the frame holds.
    #[test]
    fn the_secrets_panel_keeps_its_warning_however_many_there_are() {
        let mut m = secrets_model();
        m.secrets.rows = (0..200)
            .map(|n| SecretRow {
                name: format!("SECRET_{n:03}"),
                source: "prompt".to_owned(),
                goes_to: "x".repeat(300),
            })
            .collect();
        for (width, height) in [(MIN_WIDTH, MIN_HEIGHT), (80, 24), (200, 60)] {
            let text = flat(&m, width, height);
            assert!(text.contains("Values are never shown"), "{width}x{height}: {text}");
            for line in plain(&m, width, height).lines() {
                assert_eq!(line.chars().count(), width);
            }
        }
        let text = plain(&m, 80, 24);
        assert!(text.contains("SECRET_000"), "{text}");
        assert!(text.contains("more. The secrets list command"), "{text}");
    }

    #[test]
    fn a_project_with_no_secrets_says_so() {
        let mut m = secrets_model();
        m.secrets.rows.clear();
        m.secrets.note = Some(Line::plain("No secrets are recorded for this project."));
        let text = plain(&m, 80, 24);
        assert!(text.contains("No secrets are recorded"), "{text}");
        assert!(!text.contains("GOES TO"), "{text}");
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
        assert!(text.lines().last().unwrap().contains("←→Service"), "{text}");
        assert!(text.contains("so it can be wrong"), "{text}");
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

    fn overview_model() -> Model {
        let mut m = model();
        m.overview = Some(Overview {
            path: "/p/demo_shop".to_owned(),
            problem: None,
            services: ["postgres", "api", "worker", "web"]
                .iter()
                .map(|name| Entry::new(*name, "healthy", Style::WindowGood))
                .collect(),
            ports: vec![
                Entry::new("127.0.0.1:3000", "", Style::WindowGood),
                Entry::new("127.0.0.1:8080", "", Style::WindowGood),
            ],
            resources: vec![
                Entry::new("Memory", "6.0 GB", Style::Window),
                Entry::new("CPUs", "4", Style::Window),
            ],
            blocked: vec![
                Entry::new("6 refused lookups", "", Style::WindowAlert),
                Entry::new("of 3 hostnames", "", Style::Window),
                Entry::new("t to see them", "", Style::Window),
            ],
            blocked_alert: true,
        });
        m
    }

    #[test]
    fn the_projects_panel_has_a_list_and_a_box_for_each_part() {
        let m = overview_model();
        let text = plain(&m, 100, 30);
        for phrase in [
            "─ Services ",
            "─ Ports ",
            "─ Resources ",
            "─ Blocked ",
            "127.0.0.1:3000",
            "postgres",
            "healthy",
            "6.0 GB",
            "6 refused lookups",
            "of 3 hostnames",
            "t to see them",
            "/p/demo_shop",
            "these came from inside the sandbox, so they can be wrong",
        ] {
            assert!(text.contains(phrase), "{phrase}: {text}");
        }
        // The list is narrow: both names sit to the left of the boxes.
        let first_box = text.lines().find(|l| l.contains("─ Services")).unwrap();
        let at = first_box.chars().position(|c| c == '┌').unwrap();
        assert!(at < 40, "{first_box}");
        assert!(text.contains("► demo-shop-ab12cd34"), "{text}");
        assert!(text.contains("■ notes-ff00ff00"), "{text}");
    }

    #[test]
    fn refused_lookups_are_drawn_in_alert_colour() {
        let rows = render(&overview_model(), 100, 30);
        let spans: Vec<&Span> = rows.iter().flatten().collect();
        assert!(spans.iter().any(|s| s.style == Style::FrameAlert));
        assert!(spans
            .iter()
            .any(|s| s.style == Style::WindowAlert && s.text.contains("6 refused")));
        let mut quiet = overview_model();
        let overview = quiet.overview.as_mut().unwrap();
        overview.blocked_alert = false;
        overview.blocked = vec![Entry::new("No hostnames recorded so far", "", Style::Window)];
        let rows = render(&quiet, 100, 30);
        assert!(!rows.iter().flatten().any(|s| s.style == Style::FrameAlert));
    }

    /// On the smallest screen the boxes stack and the least needed go, but
    /// what was blocked always stays, and the frame holds.
    #[test]
    fn the_projects_panel_degrades_on_a_small_screen() {
        let m = overview_model();
        let text = plain(&m, MIN_WIDTH, MIN_HEIGHT);
        assert!(text.contains("Blocked"), "{text}");
        assert!(text.contains("6 refused"), "{text}");
        for line in text.lines() {
            assert_eq!(line.chars().count(), MIN_WIDTH);
        }
        // Services, when they fit, always bring the caveat with them.
        for height in MIN_HEIGHT..40 {
            let text = plain(&m, MIN_WIDTH, height);
            if text.contains("─ Services") {
                assert!(text.contains("these came from inside"), "{height}: {text}");
            }
        }
        assert!(plain(&m, MIN_WIDTH, 30).contains("─ Services"));
    }

    #[test]
    fn a_box_with_more_than_fits_says_how_many_more() {
        let mut m = overview_model();
        m.overview.as_mut().unwrap().services = (0..40)
            .map(|n| Entry::new(format!("svc-{n:02}"), "running", Style::WindowGood))
            .collect();
        let text = plain(&m, 100, 24);
        assert!(text.contains("svc-00"), "{text}");
        assert!(text.contains("more"), "{text}");
        assert!(!text.contains("svc-39"), "{text}");
    }

    #[test]
    fn a_hostile_service_name_cannot_break_a_box() {
        let mut m = overview_model();
        m.overview.as_mut().unwrap().services[0] =
            Entry::new("s".repeat(500), "h".repeat(500), Style::WindowWarn);
        for (width, height) in [(MIN_WIDTH, MIN_HEIGHT), (80, 24), (100, 30), (200, 60)] {
            let text = plain(&m, width, height);
            for line in text.lines() {
                assert_eq!(line.chars().count(), width, "{width}x{height}");
            }
            assert!(text.contains('║'));
        }
    }

    #[test]
    fn what_stops_a_start_is_shown_in_place_of_the_boxes() {
        let mut m = overview_model();
        m.detail = vec![Line::styled("needs a Compose file", Style::WindowAlert)];
        let text = plain(&m, 100, 30);
        assert!(text.contains("needs a Compose file"), "{text}");
        assert!(!text.contains("─ Services"), "{text}");
    }

    #[test]
    fn a_daemon_that_did_not_answer_is_said_in_place_of_the_boxes() {
        let mut m = model();
        m.overview = Some(Overview {
            problem: Some("The daemon did not answer in time.".to_owned()),
            ..Overview::default()
        });
        let text = plain(&m, 100, 30);
        assert!(text.contains("did not answer"), "{text}");
        assert!(!text.contains("─ Blocked"), "{text}");
    }

    /// Every panel, full of data, at every size from the smallest up: nothing
    /// panics and every row is exactly the width.
    #[test]
    fn every_panel_draws_at_every_size() {
        let mut m = overview_model();
        m.logs.lines = (0..300).map(|n| format!("line {n}")).collect();
        m.logs.services = vec!["api".to_owned(), "web".to_owned()];
        m.traffic.rows = (0..300).map(|n| format!("row {n}")).collect();
        m.message = Some("something happened".to_owned());
        for pane in Pane::ALL {
            m.pane = PaneState(pane);
            for width in (MIN_WIDTH - 5..=130).step_by(7) {
                for height in (MIN_HEIGHT - 3..=45).step_by(3) {
                    let rows = render(&m, width, height);
                    assert_eq!(rows.len(), height.max(MIN_HEIGHT));
                    for row in rows {
                        let drawn: usize = row.iter().map(|s| s.text.chars().count()).sum();
                        assert_eq!(drawn, width.max(MIN_WIDTH), "{}", pane.label());
                    }
                }
            }
        }
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
    fn a_start_shows_its_steps_as_they_arrive_newest_last() {
        let mut m = model();
        m.progress = Some(Progress::new("Starting demo-shop"));
        let text = plain(&m, 90, 26);
        assert!(text.contains("╡ Starting demo-shop ╞"), "{text}");
        assert!(text.contains("Waiting for the first step"), "{text}");

        let progress = m.progress.as_mut().unwrap();
        for n in 0..40 {
            progress.push(&format!("step {n:02}"), Some("detail"));
        }
        let text = plain(&m, 90, 26);
        assert!(text.contains("► step 39: detail"), "{text}");
        assert!(text.contains("√ step 38: detail"), "{text}");
        assert!(!text.contains("step 00"), "old steps scroll away: {text}");
        let rows = render(&m, 90, 26);
        assert!(rows.iter().any(|r| r.iter().any(|s| s.style == Style::Dialog)));
    }

    /// A step can carry text from the project or the sandbox.
    #[test]
    fn a_hostile_step_is_escaped_flattened_and_cut() {
        let mut progress = Progress::new("Starting");
        progress.push("build", Some("\u{1b}[2Jline one\nline two\u{7}"));
        progress.push(&"x".repeat(100_000), None);
        let first = &progress.steps[0];
        assert!(first.chars().all(|c| !c.is_control()), "{first:?}");
        assert!(first.contains("line one line two"), "{first:?}");
        assert!(progress.steps[1].chars().count() <= Progress::LONGEST);
        for _ in 0..1000 {
            progress.push("again", None);
        }
        assert_eq!(progress.steps.len(), Progress::KEPT);

        let mut m = model();
        m.progress = Some(progress);
        m.progress.as_mut().unwrap().push(&"y".repeat(500), None);
        for (width, height) in [(MIN_WIDTH, MIN_HEIGHT), (80, 24), (200, 60)] {
            for line in plain(&m, width, height).lines() {
                assert_eq!(line.chars().count(), width);
            }
        }
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

    fn bar(m: &Model, width: usize) -> String {
        plain(m, width, 24).lines().last().unwrap().to_owned()
    }

    /// Only keys that do something on the panel showing, and the keys that act
    /// on a sandbox only where the project list is.
    #[test]
    fn each_panel_shows_only_its_own_keys() {
        let mut m = model();
        let projects = bar(&m, 100);
        for shown in ["2Start", "3Stop", "xDestroy", "eShell", "5Refresh", "1Help", "10Quit"] {
            assert!(projects.contains(shown), "{shown}: {projects}");
        }
        assert!(!projects.contains("Follow"), "{projects}");

        m.pane = PaneState(Pane::Logs);
        let logs = bar(&m, 100);
        for shown in ["←→Service", "↑↓Scroll", "fFollow", "eShell", "5Refresh"] {
            assert!(logs.contains(shown), "{shown}: {logs}");
        }
        for hidden in ["Start", "Stop", "Destroy"] {
            assert!(!logs.contains(hidden), "{hidden}: {logs}");
        }

        m.pane = PaneState(Pane::Traffic);
        let traffic = bar(&m, 100);
        assert!(traffic.contains("↑↓Scroll") && traffic.contains("5Refresh"), "{traffic}");
        for hidden in ["Start", "Shell", "Service", "Follow"] {
            assert!(!traffic.contains(hidden), "{hidden}: {traffic}");
        }
    }

    /// On the narrowest screen the bar still fits, still starts with help and
    /// still ends with quitting.
    #[test]
    fn the_key_bar_fits_the_narrowest_screen_with_help_and_quit() {
        for pane in Pane::ALL {
            let mut m = model();
            m.pane = PaneState(pane);
            let line = bar(&m, MIN_WIDTH);
            assert_eq!(line.chars().count(), MIN_WIDTH);
            assert!(line.trim_start().starts_with("1Help"), "{line}");
            assert!(line.trim_end().ends_with("10Quit"), "{line}");
        }
    }

    /// Help names the keys the bar shows, so the two cannot drift apart.
    #[test]
    fn help_mentions_what_the_bar_offers() {
        for pane in Pane::ALL {
            let help = help_for(pane);
            for (key, _) in keys_for(pane) {
                let named = match *key {
                    "←→" => "Left and right",
                    "↑↓" => "Up",
                    "6" => continue,
                    "10" => continue,
                    "1" => continue,
                    other => other,
                };
                assert!(help.contains(named), "{}: {named}: {help}", pane.label());
            }
        }
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
