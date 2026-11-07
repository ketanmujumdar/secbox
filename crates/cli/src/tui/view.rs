//! What the interface looks like, worked out as a grid of coloured cells.
//!
//! The layout follows the design: a bar along the top with the program's
//! name and the early-build tag, the projects down a rail on the left, and on
//! the right the selected project's name, state and folder over a row of
//! tabs (Overview, Logs, Network, Changes, Secrets). With no project yet, the
//! whole page offers the two ways to bring one in. The bar along the bottom
//! lists only the keys that work where you are, then whether the daemon
//! answered.
//!
//! This module touches no terminal. It takes a model and a size and returns
//! the cells, which is what makes the layout testable. Every string in it
//! that came from a project or a sandbox arrives already escaped, because it
//! came through the daemon; cutting it to the width is the only other thing
//! needed to keep a hostile name from breaking the page.

use super::screen::{cut, Rect, Screen, Span};
use super::style::Style;

/// The tabs of the selected project.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Overview,
    Logs,
    Network,
    Changes,
    Secrets,
}

impl Tab {
    pub const ALL: [Tab; 5] = [
        Tab::Overview,
        Tab::Logs,
        Tab::Network,
        Tab::Changes,
        Tab::Secrets,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Logs => "Logs",
            Tab::Network => "Network",
            Tab::Changes => "Changes",
            Tab::Secrets => "Secrets",
        }
    }

    /// The letter that opens it, which is drawn in front of its name.
    pub fn key(self) -> char {
        match self {
            Tab::Overview => 'o',
            Tab::Logs => 'l',
            Tab::Network => 'n',
            Tab::Changes => 'c',
            Tab::Secrets => 's',
        }
    }

    pub fn next(self) -> Self {
        self.step(1)
    }

    pub fn previous(self) -> Self {
        self.step(Self::ALL.len() - 1)
    }

    fn step(self, by: usize) -> Self {
        let all = Self::ALL;
        let at = all.iter().position(|t| *t == self).unwrap_or(0);
        all[(at + by) % all.len()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    Running,
    Stopped,
    None,
    Unknown,
}

/// What can be done to a sandbox in a state. A key that cannot work is not
/// offered, and pressing it does nothing. When the state is unknown,
/// everything is offered and the daemon decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offers {
    pub start: bool,
    pub stop: bool,
    pub destroy: bool,
    pub shell: bool,
}

impl RowState {
    fn word(self) -> &'static str {
        match self {
            RowState::Running => "running",
            RowState::Stopped => "stopped",
            RowState::None => "no sandbox",
            RowState::Unknown => "state unknown",
        }
    }

    /// A mark for the list, where a word would take the room a name needs.
    fn mark(self) -> char {
        match self {
            RowState::Running => '●',
            RowState::Stopped => '○',
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

    pub fn offers(self) -> Offers {
        match self {
            RowState::Running => Offers {
                start: false,
                stop: true,
                destroy: true,
                shell: true,
            },
            RowState::Stopped => Offers {
                start: true,
                stop: false,
                destroy: true,
                shell: false,
            },
            RowState::None => Offers {
                start: true,
                stop: false,
                destroy: false,
                shell: false,
            },
            RowState::Unknown => Offers {
                start: true,
                stop: true,
                destroy: true,
                shell: true,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub path: String,
    pub state: RowState,
}

/// A line of text with its colour.
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

    /// A heading inside a box or a question.
    pub fn heading(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: Style::Title,
        }
    }
}

/// One line in a box on the Overview tab: a name on the left and, if there
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

/// What the Overview tab shows about the selected project, from its status
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
    /// How many hostnames had lookups refused, when the daemon is recording
    /// and something was. Shown on the Network tab. `None` is never drawn as
    /// zero, because zero would claim nothing was tried.
    pub blocked_hosts: Option<u64>,
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
        if let Some(line) = proto::person_step_line(step, detail) {
            self.steps.push(cut(&proto::text::escape(&line), Self::LONGEST));
            return;
        }
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

/// A question waiting for an answer, or a report, drawn over everything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    pub title: String,
    pub lines: Vec<Line>,
    /// Said in place of the Yes and No buttons, for a box that asks nothing.
    pub footer: Option<String>,
    /// How many lines down the box has been scrolled.
    pub scroll: usize,
    /// Whether every line has been on screen. Yes is taken only then, so a
    /// plan too long for the terminal cannot be agreed to half read.
    pub seen_all: bool,
}

impl Dialog {
    /// A box that waits for Yes or No. No is the default.
    pub fn question(title: impl Into<String>, lines: Vec<Line>) -> Self {
        Self {
            title: title.into(),
            lines,
            footer: None,
            scroll: 0,
            seen_all: false,
        }
    }

    /// A box that only reports, closed by any key.
    pub fn notice(title: impl Into<String>, lines: Vec<Line>, footer: impl Into<String>) -> Self {
        Self {
            footer: Some(footer.into()),
            ..Self::question(title, lines)
        }
    }

    pub fn asks(&self) -> bool {
        self.footer.is_none()
    }

    /// Moves through the lines on a screen of this size, stopping at each end.
    pub fn scroll_by(&mut self, delta: isize, width: usize, height: usize) {
        let layout = dialog_layout(self, width, height);
        let furthest = layout.lines.len().saturating_sub(layout.rows);
        self.scroll = self.scroll.saturating_add_signed(delta).min(furthest);
    }

    /// Records that the box was drawn on a screen of this size, and so
    /// whether its last line has now been shown.
    pub fn note_seen(&mut self, width: usize, height: usize) {
        let layout = dialog_layout(self, width, height);
        if self.scroll + layout.rows >= layout.lines.len() {
            self.seen_all = true;
        }
    }
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

/// What the Logs tab shows for the selected project.
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
        self.chosen = next
            .checked_sub(1)
            .map(|index| self.services[index].clone());
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

/// One secret on the Secrets tab. Never its value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretRow {
    pub name: String,
    /// How it was stored: typed at a prompt, or imported from a file.
    pub source: String,
    /// The services it is assigned to, and how it reaches them.
    pub goes_to: String,
}

/// What the Secrets tab shows for the selected project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretsPanel {
    pub rows: Vec<SecretRow>,
    /// Why there are no rows, when there are none.
    pub note: Option<Line>,
    /// Where the secrets could be sent, from the daemon. Already escaped.
    pub reach: Vec<String>,
}

/// What the Network tab shows for the selected project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrafficPanel {
    /// The project all of this belongs to.
    pub project: String,
    /// What has to be said about the list. Never scrolled away.
    pub notes: Vec<Line>,
    /// One blocked hostname per row, the newest last.
    pub rows: Vec<String>,
    /// The hostname on each row, unformatted, for approving one.
    pub hosts: Vec<String>,
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

/// What the Changes tab shows for the selected project: the lines of a
/// comparison with its snapshot, flags first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewPanel {
    /// The project all of this belongs to.
    pub project: String,
    /// Already escaped, with flags in alert colour.
    pub lines: Vec<Line>,
    /// How many lines down from the top the view has been scrolled.
    pub scroll: usize,
    /// How many paths changed, from the last comparison that worked.
    pub files: Option<usize>,
    /// How many of the findings need a person's attention.
    pub flags: usize,
}

impl ReviewPanel {
    /// Clears everything if it belongs to another project, so that one
    /// project's changes are never drawn under another's name.
    pub fn belong_to(&mut self, project: &str) {
        if self.project != project {
            *self = Self {
                project: project.to_owned(),
                ..Self::default()
            };
        }
    }

    /// Moves down through the lines, or back up, stopping at each end.
    pub fn scroll_by(&mut self, delta: isize) {
        let furthest = self.lines.len().saturating_sub(1);
        self.scroll = self.scroll.saturating_add_signed(delta).min(furthest);
    }
}

/// Whether the daemon answered the last time it was asked.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Daemon {
    #[default]
    Unknown,
    Up,
    Down,
}

/// The two ways to bring a project in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Choice {
    #[default]
    Git,
    Folder,
}

/// The page that offers the two ways to bring a project in. It is the whole
/// page when there is no project yet, and the right side after New project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewProject {
    /// The box marked, which Enter opens.
    pub chosen: Choice,
    /// Whether a line is being typed into the chosen box.
    pub typing: bool,
    /// The SSH remote typed so far. Printable ASCII only.
    pub remote: String,
    /// The folder typed so far. No control characters.
    pub folder: String,
    /// The folder this was started in, when it looks like a project. It is
    /// what the folder line starts with.
    pub offered: Option<String>,
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
    pub tab: Tab,
    pub dialog: Option<Dialog>,
    pub progress: Option<Progress>,
    pub logs: LogsPanel,
    pub traffic: TrafficPanel,
    pub secrets: SecretsPanel,
    pub review: ReviewPanel,
    pub daemon: Daemon,
    /// Showing the ways to bring a project in. Always set while there is
    /// no project.
    pub new_project: Option<NewProject>,
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

    /// What the selected project's state allows.
    pub fn offers(&self) -> Offers {
        self.selected_row().map_or(
            Offers {
                start: false,
                stop: false,
                destroy: false,
                shell: false,
            },
            |row| row.state.offers(),
        )
    }
}

/// One entry in the bar along the bottom: the key, then what it does.
pub type KeyLabel = (&'static str, &'static str);

/// The bar along the bottom. It lists only what works where you are, so that
/// nothing on it is a key that does nothing: the keys that act on a sandbox
/// appear only on Overview, and only those its state allows. The tabs and
/// New project have their letters on the page itself, so they are not here.
pub fn keys_for(model: &Model) -> Vec<KeyLabel> {
    if model.progress.is_some() {
        return Vec::new();
    }
    if let Some(dialog) = &model.dialog {
        return if dialog.asks() {
            vec![("y", "Yes"), ("n", "No"), ("↑↓", "Scroll")]
        } else {
            vec![("any key", "Close")]
        };
    }
    if model.projects.is_empty() || model.new_project.is_some() {
        let typing = model.new_project.as_ref().filter(|n| n.typing);
        if let Some(new) = typing {
            let submit = match new.chosen {
                Choice::Git => "Fetch",
                Choice::Folder => "Plan",
            };
            return vec![("↵", submit), ("Esc", "Cancel")];
        }
        let mut keys = vec![
            ("g", "Git"),
            ("f", "Folder"),
            ("←→", "Choose"),
            ("↵", "Continue"),
        ];
        if !model.projects.is_empty() {
            keys.push(("Esc", "Back"));
        }
        keys.extend([("?", "Help"), ("q", "Quit")]);
        return keys;
    }
    let offers = model.offers();
    let mut keys: Vec<KeyLabel> = Vec::new();
    match model.tab {
        Tab::Overview => {
            keys.push(("↑↓", "Project"));
            if offers.start {
                keys.push(("u", "Start"));
            }
            if offers.stop {
                keys.push(("S", "Stop"));
            }
            if offers.destroy {
                keys.push(("x", "Destroy"));
            }
            if offers.shell {
                keys.push(("e", "Shell"));
            }
            keys.push(("r", "Refresh"));
        }
        Tab::Logs => {
            keys.extend([("↑↓", "Scroll"), ("←→", "Service"), ("f", "Follow")]);
            if offers.shell {
                keys.push(("e", "Shell"));
            }
            keys.push(("r", "Refresh"));
        }
        Tab::Network => {
            keys.push(("↑↓", "Scroll"));
            if !model.traffic.hosts.is_empty() {
                keys.push(("a", "Approve"));
            }
            keys.push(("r", "Refresh"));
        }
        Tab::Changes => keys.extend([
            ("↑↓", "Scroll"),
            ("r", "Compare"),
            ("p", "Pull"),
            ("P", "Push"),
        ]),
        Tab::Secrets => keys.extend([("↑↓", "Project"), ("r", "Refresh")]),
    }
    keys.extend([("?", "Help"), ("q", "Quit")]);
    keys
}

/// What `?` says where you are. It names the same keys as the bar, in words.
pub fn help_for(model: &Model) -> &'static str {
    if model.projects.is_empty() || model.new_project.is_some() {
        return "g clones an SSH remote with your key on this machine; f adds a folder. Left and right choose, Enter continues, Esc goes back. Either way you see the plan, and nothing starts until you press y.";
    }
    match model.tab {
        Tab::Overview => "Up and down choose a project. u starts it after showing the plan, S stops and x destroys after asking, e opens a shell, r reads again. Letters on the tabs open them, Tab moves on, + or N adds a project, q quits.",
        Tab::Logs => "New lines follow by themselves. Left and right choose a service or All. Up and PgUp scroll back and pause; f or End follows again. e opens a shell in the chosen service. r reads again.",
        Tab::Network => "Hostnames this sandbox was refused. Up, down, PgUp and PgDn scroll. a approves the bottom row's hostname, keeping the sandbox's data. r reads the newest.",
        Tab::Changes => "r compares the sandbox's files with the snapshot it started from, inside the running sandbox. Flags come first, in red. Up, down, PgUp and PgDn scroll. p pulls and P pushes the current branch, with your SSH key on this machine. Nothing is copied out.",
        Tab::Secrets => "Names and where they go, never values. Up and down choose the project, r reads again. Change them with the secrets command.",
    }
}

const MIN_WIDTH: usize = 60;
const MIN_HEIGHT: usize = 14;
/// The most lines a message takes above the bottom bar.
const NOTE_LINES: usize = 3;

/// Draws the whole screen.
pub fn render(model: &Model, width: usize, height: usize) -> Vec<Vec<Span>> {
    compose(model, width, height).rows()
}

/// The same drawing, as a grid, so a test can read it as plain text.
pub fn compose(model: &Model, width: usize, height: usize) -> Screen {
    let width = width.max(MIN_WIDTH);
    let height = height.max(MIN_HEIGHT);
    let mut screen = Screen::new(width, height, ' ', Style::Window);

    top_bar(&mut screen);
    status_bar(&mut screen, &keys_for(model), model.daemon);

    // Between the bars, less one line for what just happened.
    let area = Rect::new(0, 1, width, height - 3);
    if model.projects.is_empty() {
        let fallback = NewProject::default();
        let new = model.new_project.as_ref().unwrap_or(&fallback);
        choices(
            new,
            false,
            &mut screen,
            Rect::new(2, area.y, width - 4, area.h),
        );
    } else {
        let rail_w = (width / 5).clamp(18, 28);
        rail(model, &mut screen, Rect::new(0, area.y, rail_w, area.h));
        screen.column_rule(rail_w, area.y, area.h, Style::Frame);
        let right = Rect::new(rail_w + 2, area.y, width - rail_w - 3, area.h);
        match &model.new_project {
            Some(new) => choices(new, true, &mut screen, right),
            None => project(model, &mut screen, right),
        }
    }

    // The line above the bottom bar is for what just happened, or what is
    // happening now. The standing warning has its own place in the top bar.
    // A long one, such as help, takes up to three lines, over the bottom of
    // the page, rather than being cut where it starts to matter.
    if let Some(note) = model.busy.as_ref().or(model.message.as_ref()) {
        let mut pieces = wrap(note, width - 4);
        if pieces.len() > NOTE_LINES {
            pieces.truncate(NOTE_LINES);
            let last = pieces[NOTE_LINES - 1].clone();
            pieces[NOTE_LINES - 1] = cut(&format!("{last} …"), width - 4);
        }
        let top = height - 1 - pieces.len().max(1);
        screen.fill(0, top, width, pieces.len().max(1), ' ', Style::WindowWarn);
        screen.text(1, top, "›", Style::Accent);
        for (offset, piece) in pieces.iter().enumerate() {
            screen.text_cut(3, top + offset, piece, width - 4, Style::WindowWarn);
        }
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
/// top bar, so that it is on screen at all times, over every tab and box.
const WARNINGS: [&str; 3] = [
    "Early build. Do not rely on it for untrusted code.",
    "Early build. Do not rely on it.",
    "Early build",
];

fn top_bar(screen: &mut Screen) {
    let width = screen.width();
    screen.fill(0, 0, width, 1, ' ', Style::MenuBar);
    let name = brand::CLI_BIN;
    screen.text(1, 0, name, Style::MenuBar);
    let room = width.saturating_sub(name.chars().count() + 4);
    let warning = WARNINGS
        .iter()
        .find(|text| text.chars().count() + 2 <= room)
        .unwrap_or(&WARNINGS[WARNINGS.len() - 1]);
    let tag = format!(" {warning} ");
    let tag_x = width.saturating_sub(tag.chars().count() + 1);
    screen.text(tag_x, 0, &tag, Style::MenuAlert);
}

/// Draws the keys, then whether the daemon answered and how to quit at the
/// right. When the keys do not all fit, the gaps close up first, then the
/// least needed go, from just before Help: the first key, Help and Quit stay.
fn status_bar(screen: &mut Screen, keys: &[KeyLabel], daemon: Daemon) {
    let width = screen.width();
    let y = screen.height() - 1;
    screen.fill(0, y, width, 1, ' ', Style::StatusLabel);
    let quit = keys.iter().any(|(key, _)| *key == "q");
    let mut left: Vec<KeyLabel> = keys
        .iter()
        .copied()
        .filter(|(key, _)| *key != "q")
        .collect();

    let (dot, word, dot_style) = match daemon {
        Daemon::Up => ('●', "daemon", Style::StatusGood),
        Daemon::Down => ('●', "daemon off", Style::StatusAlert),
        Daemon::Unknown => ('○', "daemon", Style::StatusLabel),
    };
    let right_w = 2 + word.chars().count() + if quit { 8 } else { 0 };
    let right_x = width.saturating_sub(right_w + 1);
    let mut x = right_x;
    x += screen.text(x, y, &dot.to_string(), dot_style);
    x += screen.text(x, y, &format!(" {word}"), Style::StatusLabel);
    if quit {
        x += 2;
        x += screen.text(x, y, "q", Style::StatusKey);
        screen.text(x, y, " Quit", Style::StatusLabel);
    }

    let room = right_x.saturating_sub(3);
    let needed = |keys: &[KeyLabel], gap: usize| -> usize {
        let text: usize = keys
            .iter()
            .map(|(key, word)| key.chars().count() + 1 + word.chars().count())
            .sum();
        text + gap * keys.len().saturating_sub(1)
    };
    let mut gap = 3;
    while needed(&left, gap) > room && gap > 1 {
        gap -= 1;
    }
    while needed(&left, gap) > room && left.len() > 2 {
        let help = left
            .iter()
            .position(|(key, _)| *key == "?")
            .unwrap_or(left.len());
        let at = help.saturating_sub(1).max(1);
        if at >= left.len() {
            break;
        }
        left.remove(at);
    }
    let mut x = 1;
    for (key, word) in left {
        if x > room {
            break;
        }
        x += screen.text(x, y, key, Style::StatusKey);
        x += screen.text(x, y, &format!(" {word}"), Style::StatusLabel);
        x += gap;
    }
}

/// New project, then one row per project: a mark for its state and its
/// name. The mark is in the state's colour, and the selected row is
/// highlighted rather than marked.
fn rail(model: &Model, screen: &mut Screen, at: Rect) {
    let adding = model.new_project.is_some();
    let line_w = at.w;
    if adding {
        screen.fill(at.x, at.y, line_w, 1, ' ', Style::Selected);
    }
    let (plus, words) = if adding {
        (Style::Selected, Style::Selected)
    } else {
        (Style::TabKey, Style::Accent)
    };
    screen.text(at.x + 1, at.y, "+", plus);
    screen.text_cut(
        at.x + 3,
        at.y,
        "New project",
        line_w.saturating_sub(4),
        words,
    );
    if at.h < 4 {
        return;
    }
    screen.text(at.x + 1, at.y + 2, "PROJECTS", Style::WindowDim);
    let list_y = at.y + 3;
    let list_h = at.h - 3;
    let first = model.selected.saturating_sub(list_h.saturating_sub(1));
    let overflow = model.projects.len() > list_h;
    let row_w = if overflow { line_w - 1 } else { line_w };
    for offset in 0..list_h {
        let Some(row) = model.projects.get(first + offset) else {
            break;
        };
        let selected = !adding && first + offset == model.selected;
        let y = list_y + offset;
        if selected {
            screen.fill(at.x, y, row_w, 1, ' ', Style::Selected);
        }
        let (name_style, mark_style) = if selected {
            (Style::Selected, Style::Selected)
        } else {
            (Style::Window, row.state.style())
        };
        screen.put(at.x + 1, y, row.state.mark(), mark_style);
        screen.text_cut(at.x + 3, y, &row.name, row_w.saturating_sub(4), name_style);
    }
    if overflow {
        screen.scrollbar(
            at.x + at.w - 1,
            list_y,
            list_h,
            first,
            list_h,
            model.projects.len(),
        );
    }
}

/// The selected project's name, state and folder, its tabs, and the tab
/// showing.
fn project(model: &Model, screen: &mut Screen, at: Rect) {
    let Some(row) = model.selected_row() else {
        return;
    };
    header(row, screen, at);
    tabs(model, row, screen, Rect::new(at.x, at.y + 1, at.w, 1));
    screen.rule(at.x, at.y + 2, at.w, Style::Frame);
    let body = Rect::new(at.x, at.y + 3, at.w, at.h.saturating_sub(3));
    match model.tab {
        Tab::Overview => overview_body(model, screen, body),
        Tab::Logs => logs_body(model, screen, body),
        Tab::Network => traffic_body(model, screen, body),
        Tab::Changes => review_body(model, row, screen, body),
        Tab::Secrets => secrets_body(model, screen, body),
    }
}

/// The name and its state on the left, and the folder it comes from at the
/// right, which is the first to be cut when room is short.
fn header(row: &Row, screen: &mut Screen, at: Rect) {
    let word = row.state.word();
    let word_w = word.chars().count();
    let name = cut(&row.name, at.w.saturating_sub(word_w + 2).max(1));
    let name_w = name.chars().count();
    screen.text(at.x, at.y, &name, Style::Title);
    screen.text_cut(
        at.x + name_w + 2,
        at.y,
        word,
        at.w.saturating_sub(name_w + 2),
        row.state.style(),
    );
    let used = name_w + 2 + word_w + 3;
    let room = at.w.saturating_sub(used);
    if room >= 12 {
        let path = cut(&row.path, room);
        let path_w = path.chars().count();
        screen.text(at.x + at.w - path_w, at.y, &path, Style::WindowDim);
    }
}

/// One tab as drawn: its letter, its name, and a count when there is one.
struct TabItem {
    tab: Tab,
    badge: Option<(String, Style)>,
}

/// The row of tabs. The one showing is filled; on the others the letter that
/// opens them is marked. When they do not fit, the padding closes up first,
/// then the tabs not showing keep only their letter and count.
fn tabs(model: &Model, row: &Row, screen: &mut Screen, at: Rect) {
    let network = model
        .overview
        .as_ref()
        .and_then(|o| o.blocked_hosts)
        .filter(|n| *n > 0)
        .map(|n| (n.to_string(), Style::WindowAlert));
    let changes = (model.review.project == row.path)
        .then_some(model.review.files)
        .flatten()
        .map(|n| {
            let style = if model.review.flags > 0 {
                Style::WindowAlert
            } else {
                Style::WindowDim
            };
            (n.to_string(), style)
        });
    let items: Vec<TabItem> = Tab::ALL
        .iter()
        .map(|tab| TabItem {
            tab: *tab,
            badge: match tab {
                Tab::Network => network.clone(),
                Tab::Changes => changes.clone(),
                _ => None,
            },
        })
        .collect();
    let item_w = |item: &TabItem, pad: usize, full: bool| -> usize {
        let label = if full || item.tab == model.tab {
            1 + item.tab.label().len()
        } else {
            0
        };
        let badge = item
            .badge
            .as_ref()
            .map_or(0, |(text, _)| 1 + text.chars().count());
        2 * pad + 1 + label + badge
    };
    let total = |pad: usize, gap: usize, full: bool| -> usize {
        items.iter().map(|i| item_w(i, pad, full)).sum::<usize>() + gap * (items.len() - 1)
    };
    let (pad, gap, full) = [
        (1, 1, true),
        (1, 0, true),
        (0, 1, true),
        (1, 0, false),
        (0, 1, false),
    ]
    .into_iter()
    .find(|(pad, gap, full)| total(*pad, *gap, *full) <= at.w)
    .unwrap_or((0, 1, false));
    let mut x = at.x;
    for item in &items {
        let active = item.tab == model.tab;
        let w = item_w(item, pad, full);
        if x + w > at.x + at.w {
            break;
        }
        if active {
            screen.fill(x, at.y, w, 1, ' ', Style::TabActive);
        }
        let mut cx = x + pad;
        let (key_style, label_style) = if active {
            (Style::TabActive, Style::TabActive)
        } else {
            (Style::TabKey, Style::Window)
        };
        cx += screen.text(cx, at.y, &item.tab.key().to_string(), key_style);
        if full || active {
            cx += 1;
            cx += screen.text(cx, at.y, item.tab.label(), label_style);
        }
        if let Some((text, style)) = &item.badge {
            cx += 1;
            screen.text(
                cx,
                at.y,
                text,
                if active { Style::TabActive } else { *style },
            );
        }
        x += w + gap;
    }
}

/// The two ways to bring a project in, each in a box with its key, what it
/// does, and the line it is typed into. Side by side when there is room,
/// otherwise one above the other.
fn choices(new: &NewProject, beside_projects: bool, screen: &mut Screen, at: Rect) {
    let title = if beside_projects {
        "Bring another project into a sandbox"
    } else {
        "Bring a project into a sandbox"
    };
    let subtitle = wrap(
        "It runs in its own small VM. Your keys and secrets stay on this computer.",
        at.w,
    );
    let tip = wrap(
        &format!(
            "Tip: {bin} new git@… or {bin} new . does the same without this screen.",
            bin = brand::CLI_BIN
        ),
        at.w,
    );
    let side = at.w >= 2 * 34 + 3;
    let box_w = if side {
        ((at.w - 3) / 2).min(46)
    } else {
        at.w.min(64)
    };
    let inner = box_w.saturating_sub(4);
    let git_text = "Paste an SSH remote. It is fetched here with your key; the key never goes in.";
    let folder_text = format!(
        "Type a path, or start {} inside the folder and it is offered here.",
        brand::CLI_BIN
    );
    let mut git_desc = wrap(git_text, inner);
    let mut folder_desc = wrap(&folder_text, inner);

    let box_h = |git: &[String], folder: &[String]| -> usize {
        let desc = if side { git.len().max(folder.len()) } else { 0 };
        if side {
            5 + desc
        } else {
            10 + git.len() + folder.len()
        }
    };
    let mut show_sub = true;
    let mut show_tip = true;
    let total = |sub: bool, tip_on: bool, git: &[String], folder: &[String]| -> usize {
        1 + if sub { subtitle.len() } else { 0 }
            + 1
            + box_h(git, folder)
            + if tip_on { 1 + tip.len() } else { 0 }
    };
    if total(show_sub, show_tip, &git_desc, &folder_desc) > at.h {
        show_tip = false;
    }
    if total(show_sub, show_tip, &git_desc, &folder_desc) > at.h {
        show_sub = false;
    }
    while total(show_sub, show_tip, &git_desc, &folder_desc) > at.h
        && (git_desc.len() > 1 || folder_desc.len() > 1)
    {
        git_desc.truncate(git_desc.len().saturating_sub(1).max(1));
        folder_desc.truncate(folder_desc.len().saturating_sub(1).max(1));
    }
    let height = total(show_sub, show_tip, &git_desc, &folder_desc);
    let mut y = at.y + at.h.saturating_sub(height) / 2;
    let centre = |screen: &mut Screen, y: usize, text: &str, style: Style| {
        let shown = cut(text, at.w);
        let x = at.x + (at.w.saturating_sub(shown.chars().count())) / 2;
        screen.text(x, y, &shown, style);
    };
    centre(screen, y, title, Style::Title);
    y += 1;
    if show_sub {
        for line in &subtitle {
            centre(screen, y, line, Style::WindowDim);
            y += 1;
        }
    }
    y += 1;

    let git_field = if (new.typing && new.chosen == Choice::Git) || !new.remote.is_empty() {
        Field::Text(&new.remote)
    } else {
        Field::Hint("git@github.com:org/repo.git")
    };
    let folder_field = if (new.typing && new.chosen == Choice::Folder) || !new.folder.is_empty() {
        Field::Text(&new.folder)
    } else if let Some(offered) = &new.offered {
        Field::Text(offered)
    } else {
        Field::Hint("~/code/my-project")
    };
    let git = ChoiceBox {
        key: "g",
        title: "Clone from Git",
        desc: &git_desc,
        field: git_field,
        chosen: new.chosen == Choice::Git,
        typing: new.typing && new.chosen == Choice::Git,
    };
    let folder = ChoiceBox {
        key: "f",
        title: "Add a folder",
        desc: &folder_desc,
        field: folder_field,
        chosen: new.chosen == Choice::Folder,
        typing: new.typing && new.chosen == Choice::Folder,
    };
    if side {
        let h = box_h(&git_desc, &folder_desc);
        let start = at.x + (at.w.saturating_sub(2 * box_w + 3)) / 2;
        draw_choice(screen, Rect::new(start, y, box_w, h), &git);
        draw_choice(screen, Rect::new(start + box_w + 3, y, box_w, h), &folder);
        y += h;
    } else {
        let start = at.x + (at.w.saturating_sub(box_w)) / 2;
        let git_h = 5 + git_desc.len();
        draw_choice(screen, Rect::new(start, y, box_w, git_h), &git);
        y += git_h;
        let folder_h = 5 + folder_desc.len();
        draw_choice(screen, Rect::new(start, y, box_w, folder_h), &folder);
        y += folder_h;
    }
    if show_tip {
        y += 1;
        for line in &tip {
            centre(screen, y, line, Style::WindowDim);
            y += 1;
        }
    }
}

/// What a line to type into shows.
enum Field<'a> {
    /// Typed, or offered, text. Escaped before it is drawn.
    Text(&'a str),
    /// What could go there, shown dim until something is typed.
    Hint(&'static str),
}

struct ChoiceBox<'a> {
    key: &'static str,
    title: &'static str,
    desc: &'a [String],
    field: Field<'a>,
    chosen: bool,
    typing: bool,
}

fn draw_choice(screen: &mut Screen, at: Rect, choice: &ChoiceBox) {
    if at.w < 8 || at.h < 5 {
        return;
    }
    let frame = if choice.chosen {
        Style::Accent
    } else {
        Style::Frame
    };
    screen.group_box(at, frame, "", Style::Title);
    let inner = at.w - 4;
    let x = at.x + 2;
    let mut y = at.y + 1;
    let badge = if choice.chosen {
        Style::TabActive
    } else {
        Style::Badge
    };
    screen.text(x, y, &format!(" {} ", choice.key), badge);
    screen.text_cut(
        x + 4,
        y,
        choice.title,
        inner.saturating_sub(4),
        Style::Title,
    );
    y += 1;
    for line in choice.desc {
        screen.text_cut(x, y, line, inner, Style::WindowDim);
        y += 1;
    }
    y += 1;
    screen.fill(x, y, inner, 1, ' ', Style::Field);
    let room = inner.saturating_sub(2);
    match &choice.field {
        Field::Text(text) => {
            let shown = proto::text::escape(text);
            let shown = if choice.typing {
                format!("{shown}_")
            } else {
                shown
            };
            // The end of what is typed is the part being worked on, so a
            // long line keeps its end in view.
            let count = shown.chars().count();
            let tail: String = if count > room {
                std::iter::once('…')
                    .chain(shown.chars().skip(count - room + 1))
                    .collect()
            } else {
                shown
            };
            screen.text(x + 1, y, &tail, Style::Field);
        }
        Field::Hint(hint) => screen.text_cut(x + 1, y, hint, room, Style::FieldDim),
    }
}

/// The selected project's services, ports, resources and blocked lookups in
/// boxes. On a small screen the boxes stack, and the ones that matter least
/// go first.
fn overview_body(model: &Model, screen: &mut Screen, at: Rect) {
    let bottom = at.y + at.h;
    let mut y = at.y;
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
        wrap(
            "these came from inside the sandbox, so they can be wrong",
            at.w,
        )
    };
    let boxes_bottom = bottom.saturating_sub(caveat.len());
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
        draw_group(
            screen,
            Rect::new(right_x, area.y, right_w, top_h),
            &resources,
        );
        draw_group(screen, Rect::new(left_x, bottom_y, half, bottom_h), &ports);
        draw_group(
            screen,
            Rect::new(right_x, bottom_y, right_w, bottom_h),
            &blocked,
        );
        return Some(bottom_y + bottom_h);
    }

    // One column. What was blocked matters most, so it gets all the room it
    // would like first. Then the others get a frame and one line each, then
    // what more each would like, in the order they matter.
    let order = [&blocked, &services, &ports, &resources];
    let mut heights = [0usize; 4];
    let mut left = area.h;
    if left >= GROUP_MIN {
        heights[0] = blocked.wanted().min(left);
        left -= heights[0];
    }
    for (index, _) in order.iter().enumerate().skip(1) {
        if left >= GROUP_MIN {
            heights[index] = GROUP_MIN;
            left -= GROUP_MIN;
        }
    }
    for (index, group) in order.iter().enumerate().skip(1) {
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
            draw_group(
                screen,
                Rect::new(area.x, y, area.w, heights[index]),
                order[index],
            );
            y += heights[index];
        }
    }
    (heights[1] > 0).then_some(y)
}

/// A single-ruled box with its title in the top rule. Each entry is a line,
/// its second part set to the right. When they do not all fit, the last line
/// says how many more.
fn draw_group(screen: &mut Screen, at: Rect, group: &Group) {
    if at.w < 6 || at.h < GROUP_MIN {
        return;
    }
    let (frame, title) = if group.alert {
        (Style::FrameAlert, Style::WindowAlert)
    } else {
        (Style::Frame, Style::WindowDim)
    };
    screen.group_box(at, frame, &group.title.to_uppercase(), title);
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
        // With two parts only the value is coloured; with one, the whole line.
        let left_style = if entry.right.is_empty() {
            entry.style
        } else {
            Style::Window
        };
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
fn logs_body(model: &Model, screen: &mut Screen, at: Rect) {
    let panel = &model.logs;
    let (x, y, w) = (at.x, at.y, at.w);
    // Whether lines are arriving is set at the right, where it stays in view
    // however the lines are scrolled; the services get what room is left.
    let (tag, tag_style) = live_tag(panel);
    let tag_w = tag.chars().count().min(w);
    screen.text_cut(x + w - tag_w, y, &tag, tag_w, tag_style);
    service_bar(panel, screen, x, y, w.saturating_sub(tag_w + 2));
    if at.h < 2 {
        return;
    }
    screen.text_cut(
        x,
        y + 1,
        "Output comes from inside the sandbox, so it can be wrong.",
        w,
        Style::WindowDim,
    );
    if at.h <= 3 {
        return;
    }
    screen.rule(x, y + 2, w, Style::Frame);
    let mut row = y + 3;
    let bottom = y + at.h;
    if let Some(note) = &panel.note {
        screen.text_cut(x, row, &note.text, w, note.style);
        row += 1;
    }
    let fits = bottom.saturating_sub(row);
    let end = panel.lines.len().saturating_sub(panel.scroll);
    let first = end.saturating_sub(fits);
    for line in &panel.lines[first..end] {
        screen.text_cut(x, row, line, w, Style::Window);
        row += 1;
    }
}

/// Says whether the tab is following, and when it is paused, how to resume
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
fn secrets_body(model: &Model, screen: &mut Screen, at: Rect) {
    let panel = &model.secrets;
    let (x, y, width) = (at.x, at.y, at.w);
    let bottom = y + at.h;

    // What is always true goes at the bottom, and is kept however many
    // secrets there are, so the rows get what is left.
    let footer: Vec<(String, Style)> = [
        (
            "Names only. Values are never shown, and the keychain was not read.",
            Style::WindowWarn,
        ),
        (
            "A service can read every secret it is given.",
            Style::WindowDim,
        ),
    ]
    .iter()
    .flat_map(|(text, style)| wrap(text, width).into_iter().map(|piece| (piece, *style)))
    .chain(
        wrap(
            &format!(
                "Add and assign them with: {} secrets set NAME --service SERVICE",
                brand::CLI_BIN
            ),
            width,
        )
        .into_iter()
        .map(|piece| (piece, Style::WindowDim)),
    )
    .chain(fill_hint(panel, width))
    .collect();
    let footer_y = bottom.saturating_sub(footer.len()).max(y);
    for (offset, (text, style)) in footer.iter().enumerate() {
        if footer_y + offset < bottom {
            screen.text_cut(x, footer_y + offset, text, width, *style);
        }
    }
    let rows_bottom = footer_y.saturating_sub(1);

    let mut row = y;
    if let Some(note) = &panel.note {
        if row < rows_bottom {
            screen.text_cut(x, row, &note.text, width, note.style);
        }
        return;
    }
    if panel.rows.is_empty() || row >= rows_bottom {
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
        format!(
            "{:<name_w$}  {:<source_w$}  {goes}",
            cut(name, name_w),
            source
        )
    };
    screen.text_cut(
        x,
        row,
        &columns("NAME", "SOURCE", "GOES TO"),
        width,
        Style::WindowDim,
    );
    row += 1;
    let fits = rows_bottom.saturating_sub(row);
    let shown = if panel.rows.len() > fits {
        fits.saturating_sub(1)
    } else {
        fits
    };
    for secret in panel.rows.iter().take(shown) {
        let style = if secret.source == EMPTY_SOURCE {
            Style::WindowWarn
        } else if secret.goes_to == "no service" {
            Style::WindowDim
        } else {
            Style::Window
        };
        screen.text_cut(
            x,
            row,
            &columns(&secret.name, &secret.source, &secret.goes_to),
            width,
            style,
        );
        row += 1;
    }
    if shown < panel.rows.len() && row < rows_bottom {
        screen.text_cut(
            x,
            row,
            &format!(
                "and {} more. The secrets list command shows them all.",
                panel.rows.len() - shown
            ),
            width,
            Style::WindowDim,
        );
        row += 1;
    }
    // Where they could go, in the room left, wrapped rather than cut so a
    // host name is never shown half missing.
    row += 1;
    for line in &panel.reach {
        for piece in wrap(line, width) {
            if row >= rows_bottom {
                return;
            }
            screen.text_cut(x, row, &piece, width, Style::WindowWarn);
            row += 1;
        }
    }
}

/// The comparison's lines from the scroll point on. Before the first
/// comparison, or after one that could not run, it says how to get one, so
/// an empty tab is never read as "nothing changed".
fn review_body(model: &Model, row: &Row, screen: &mut Screen, at: Rect) {
    let panel = &model.review;
    let (x, y, width) = (at.x, at.y, at.w);
    let bottom = y + at.h;
    let mut line_y = y;
    if panel.lines.is_empty() || panel.project != row.path {
        for (text, style) in [
            ("Press r to compare this sandbox's files with the snapshot it started from.", Style::Window),
            ("The comparison runs inside the sandbox, so it must be running. The host reviews the result and runs nothing from it. Nothing is copied out.", Style::WindowDim),
            ("p pulls and P pushes the current branch, with your SSH key on this machine. The key never enters the sandbox.", Style::WindowDim),
        ] {
            for piece in wrap(text, width) {
                if line_y < bottom {
                    screen.text_cut(x, line_y, &piece, width, style);
                    line_y += 1;
                }
            }
            line_y += 1;
        }
        return;
    }
    let overflow = panel.lines.len() > at.h;
    let text_w = if overflow {
        width.saturating_sub(2)
    } else {
        width
    };
    for line in panel.lines.iter().skip(panel.scroll) {
        if line_y >= bottom {
            break;
        }
        screen.text_cut(x, line_y, &line.text, text_w, line.style);
        line_y += 1;
    }
    if overflow {
        screen.scrollbar(
            x + width - 1,
            y,
            at.h,
            panel.scroll,
            at.h,
            panel.lines.len(),
        );
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

/// As [`wrap`], keeping the line's own indent on every piece, so a list item
/// that runs on still reads as part of its list. An empty line stays one.
///
/// A word longer than the width, such as a long host name, is broken across
/// lines rather than cut, because these lines are a plan someone is agreeing
/// to, and every character of it has to be on screen.
fn wrap_indented(text: &str, width: usize) -> Vec<String> {
    let indent: String = text.chars().take_while(|c| *c == ' ').collect();
    let room = width.saturating_sub(indent.len()).max(1);
    let pieces = wrap(text, room);
    if pieces.is_empty() {
        return vec![String::new()];
    }
    pieces
        .into_iter()
        .flat_map(|piece| {
            let chars: Vec<char> = piece.chars().collect();
            chars
                .chunks(room)
                .map(|chunk| chunk.iter().collect::<String>())
                .collect::<Vec<_>>()
        })
        .map(|piece| format!("{indent}{piece}"))
        .collect()
}

/// What the list cannot show comes first and stays there, whatever the scroll,
/// and is wrapped rather than cut. The rows get whatever room is left.
fn traffic_body(model: &Model, screen: &mut Screen, at: Rect) {
    let panel = &model.traffic;
    let (x, width) = (at.x, at.w);
    let bottom = at.y + at.h;
    let mut row = at.y;
    let say = |screen: &mut Screen, row: &mut usize, text: &str, style: Style| {
        for piece in wrap(text, width) {
            if *row < bottom {
                screen.text_cut(x, *row, &piece, width, style);
                *row += 1;
            }
        }
    };
    // An alert first, then what is always true, which needs no answer from the
    // daemon and so stands even when there is none.
    for note in panel.notes.iter().filter(|n| n.style == Style::WindowAlert) {
        say(screen, &mut row, &note.text, note.style);
    }
    say(
        screen,
        &mut row,
        crate::blocked::HOSTNAMES_ONLY,
        Style::WindowWarn,
    );
    for note in panel.notes.iter().filter(|n| n.style != Style::WindowAlert) {
        say(screen, &mut row, &note.text, note.style);
    }
    if row + 2 > bottom {
        return;
    }
    screen.rule(x, row, width, Style::Frame);
    row += 1;
    if panel.rows.is_empty() {
        return;
    }
    screen.text_cut(x, row, &crate::blocked::header(), width, Style::WindowDim);
    row += 1;
    let fits = bottom.saturating_sub(row);
    let end = panel.rows.len().saturating_sub(panel.scroll);
    let first = end.saturating_sub(fits);
    for line in &panel.rows[first..end] {
        screen.text_cut(x, row, line, width, Style::Window);
        row += 1;
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

/// The steps of a start in a box over everything, the newest at the bottom
/// and marked, the ones before it ticked off. Older steps scroll away at the
/// top, as on a printer.
fn draw_progress(screen: &mut Screen, progress: &Progress) {
    let width = (screen.width() * 3 / 4)
        .clamp(40, 76)
        .min(screen.width() - 2);
    let height =
        (progress.steps.len().max(1) + 4).clamp(6, screen.height().saturating_sub(4).max(6));
    let x = (screen.width().saturating_sub(width)) / 2;
    let y = (screen.height().saturating_sub(height)) / 2;
    let box_ = Rect::new(x, y, width, height);
    screen.fill(x, y, width, height, ' ', Style::Dialog);
    screen.frame(box_, Style::DialogFrame, &progress.title, Style::Dialog);

    let rows = height - 4;
    let first = progress.steps.len().saturating_sub(rows);
    if progress.steps.is_empty() {
        screen.text_cut(
            x + 2,
            y + 2,
            "Waiting for the first step.",
            width - 4,
            Style::Dialog,
        );
    }
    for (offset, step) in progress.steps[first..].iter().enumerate() {
        let newest = first + offset + 1 == progress.steps.len();
        let (mark, style) = if newest {
            ('►', Style::DialogHot)
        } else {
            ('✓', Style::DialogDim)
        };
        screen.put(x + 2, y + 2 + offset, mark, style);
        screen.text_cut(x + 4, y + 2 + offset, step, width - 6, Style::Dialog);
    }
}

/// Where a box goes on a screen of a size, and its lines broken to its width.
struct DialogLayout {
    at: Rect,
    lines: Vec<Line>,
    /// How many lines are shown at once.
    rows: usize,
}

fn dialog_layout(dialog: &Dialog, width: usize, height: usize) -> DialogLayout {
    let width = width.max(MIN_WIDTH);
    let height = height.max(MIN_HEIGHT);
    let box_w = (width * 9 / 10).clamp(44, 80).min(width - 2);
    // Two columns of margin each side, and one for a scrollbar.
    let text_w = box_w - 6;
    let lines: Vec<Line> = dialog
        .lines
        .iter()
        .flat_map(|line| {
            wrap_indented(&line.text, text_w)
                .into_iter()
                .map(|text| Line::styled(text, line.style))
        })
        .collect();
    // A frame, a blank line, the lines, a line for "more below", the
    // buttons and the frame again.
    // It sits between the top bar and the message line, so that what a key
    // just said, such as why Yes did nothing, is never hidden under it.
    let rows = lines.len().clamp(1, height.saturating_sub(4 + 5).max(1));
    let box_h = rows + 5;
    let x = (width - box_w) / 2;
    let y = 1 + (height.saturating_sub(3 + box_h)) / 2;
    DialogLayout {
        at: Rect::new(x, y, box_w, box_h),
        lines,
        rows,
    }
}

fn draw_dialog(screen: &mut Screen, dialog: &Dialog) {
    let layout = dialog_layout(dialog, screen.width(), screen.height());
    let Rect { x, y, w, h } = layout.at;
    screen.fill(x, y, w, h, ' ', Style::Dialog);
    screen.frame(layout.at, Style::DialogFrame, &dialog.title, Style::Dialog);
    let text_w = w - 6;
    let first = dialog
        .scroll
        .min(layout.lines.len().saturating_sub(layout.rows));
    for (offset, line) in layout
        .lines
        .iter()
        .skip(first)
        .take(layout.rows)
        .enumerate()
    {
        let style = match line.style {
            Style::Title => Style::DialogHot,
            Style::WindowAlert | Style::WindowWarn => Style::DialogAlert,
            Style::WindowDim => Style::DialogDim,
            _ => Style::Dialog,
        };
        screen.text_cut(x + 2, y + 1 + offset, &line.text, text_w, style);
    }
    let more = layout.lines.len() > layout.rows;
    if more {
        screen.scrollbar(
            x + w - 3,
            y + 1,
            layout.rows,
            first,
            layout.rows,
            layout.lines.len(),
        );
    }
    if more && first + layout.rows < layout.lines.len() {
        let hint = if dialog.asks() {
            "↓ more below. Yes works once you have seen it all."
        } else {
            "↓ more below"
        };
        screen.text_cut(x + 2, y + h - 3, hint, text_w, Style::DialogHot);
    }

    let buttons_y = y + h - 2;
    if let Some(footer) = &dialog.footer {
        let shown = cut(footer, w.saturating_sub(4));
        let start = x + (w.saturating_sub(shown.chars().count())) / 2;
        screen.text(start, buttons_y, &shown, Style::DialogHot);
        return;
    }
    // Two buttons. No is the one Enter gives, so it is the one marked; Yes
    // is greyed out until the whole question has been on screen.
    let yes = "  Yes  ";
    let no = "  No  ";
    let total = yes.len() + no.len() + 4;
    let start = x + (w.saturating_sub(total)) / 2;
    let (yes_style, yes_hot) = if dialog.seen_all {
        (Style::Dialog, Style::DialogHot)
    } else {
        (Style::DialogDim, Style::DialogDim)
    };
    screen.text(start, buttons_y, yes, yes_style);
    screen.text(start + 2, buttons_y, "Y", yes_hot);
    screen.text(start + yes.len() + 4, buttons_y, no, Style::Button);
    screen.text(start + yes.len() + 6, buttons_y, "N", Style::ButtonHot);
}

/// A key on a project's tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Open(Tab),
    NextTab,
    PreviousTab,
    Up,
    Down,
    PageUp,
    PageDown,
    Quit,
    Refresh,
    Start,
    Stop,
    Destroy,
    Approve,
    Help,
    PreviousService,
    NextService,
    /// Back to the newest line, which resumes following a log.
    Follow,
    /// A shell in the selected project's sandbox.
    Shell,
    /// Git pull in the selected project's sandbox, through the relay.
    Pull,
    /// Git push of its current branch, through the relay.
    Push,
    /// The ways to bring in another project.
    NewProject,
    Other,
}

/// Reads one keystroke on a project's tabs. The letter drawn on a tab opens
/// it and does nothing else, so a key pressed to look at something can never
/// also stop a sandbox. What acts on a sandbox is a capital or a key of its
/// own, and asks first where it cannot be undone.
pub fn key(bytes: &[u8]) -> Key {
    match bytes {
        b"\x1b[A" | b"\x1bOA" | b"k" => Key::Up,
        b"\x1b[B" | b"\x1bOB" | b"j" => Key::Down,
        b"\x1b[5~" => Key::PageUp,
        b"\x1b[6~" => Key::PageDown,
        b"f" | b"F" | b"\x1b[F" | b"\x1bOF" | b"\x1b[4~" | b"\x1b[8~" => Key::Follow,
        b"\t" => Key::NextTab,
        b"\x1b[Z" => Key::PreviousTab,
        b"q" | b"\x03" | b"\x1b[21~" => Key::Quit,
        b"o" | b"O" => Key::Open(Tab::Overview),
        b"l" | b"L" => Key::Open(Tab::Logs),
        b"n" => Key::Open(Tab::Network),
        b"c" | b"C" => Key::Open(Tab::Changes),
        b"s" => Key::Open(Tab::Secrets),
        b"+" | b"N" => Key::NewProject,
        // The function keys still work, for hands that know them.
        b"r" | b"R" | b"\x1b[15~" => Key::Refresh,
        b"u" | b"\x1bOQ" => Key::Start,
        b"S" | b"\x1bOR" => Key::Stop,
        b"x" | b"X" => Key::Destroy,
        b"a" => Key::Approve,
        b"e" | b"E" => Key::Shell,
        b"p" => Key::Pull,
        b"P" => Key::Push,
        b"[" | b"\x1b[D" | b"\x1bOD" => Key::PreviousService,
        b"]" | b"\x1b[C" | b"\x1bOC" => Key::NextService,
        b"?" | b"\x1bOP" => Key::Help,
        _ => Key::Other,
    }
}

/// A key on the page that offers the ways to bring a project in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceKey {
    Git,
    Folder,
    Left,
    Right,
    Enter,
    Back,
    Help,
    Quit,
    Other,
}

pub fn choice_key(bytes: &[u8]) -> ChoiceKey {
    match bytes {
        b"g" | b"G" => ChoiceKey::Git,
        b"f" | b"F" => ChoiceKey::Folder,
        b"\x1b[D" | b"\x1bOD" | b"h" => ChoiceKey::Left,
        b"\x1b[C" | b"\x1bOC" | b"\t" => ChoiceKey::Right,
        b"\r" | b"\n" => ChoiceKey::Enter,
        b"\x1b" => ChoiceKey::Back,
        b"?" | b"\x1bOP" => ChoiceKey::Help,
        b"q" | b"\x03" | b"\x1b[21~" => ChoiceKey::Quit,
        _ => ChoiceKey::Other,
    }
}

/// A key pressed while a question is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    Up,
    Down,
    PageUp,
    PageDown,
    Other,
}

/// Only `y` agrees. Enter, Esc, `n`, `q` and Ctrl-C all mean no, so the
/// default answer is the one that changes nothing.
pub fn answer(bytes: &[u8]) -> Answer {
    match bytes {
        b"y" | b"Y" => Answer::Yes,
        b"n" | b"N" | b"\x1b" | b"\r" | b"\n" | b"q" | b"\x03" => Answer::No,
        b"\x1b[A" | b"\x1bOA" | b"k" => Answer::Up,
        b"\x1b[B" | b"\x1bOB" | b"j" => Answer::Down,
        b"\x1b[5~" => Answer::PageUp,
        b"\x1b[6~" => Answer::PageDown,
        _ => Answer::Other,
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

/// How the Secrets panel's rows mark a name that has no value yet.
const EMPTY_SOURCE: &str = "empty";

/// A line under the Secrets panel saying how to fill in the empty names, when
/// there are any.
fn fill_hint(panel: &SecretsPanel, width: usize) -> Vec<(String, Style)> {
    let empty = panel.rows.iter().filter(|r| r.source == EMPTY_SOURCE).count();
    if empty == 0 {
        return Vec::new();
    }
    let text = format!(
        "{empty} empty, with no value yet. Fill them in with: {} secrets fill",
        brand::CLI_BIN
    );
    wrap(&text, width)
        .into_iter()
        .map(|piece| (piece, Style::WindowWarn))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_start_box_says_when_a_prompt_is_up_or_nobody_was_asked() {
        let mut progress = Progress::new("Starting shop");
        progress.push(proto::STEP_CONFIRM, Some("Touch ID or your login password"));
        progress.push(proto::STEP_UNCONFIRMED, None);
        progress.push("sandbox", None);
        assert!(progress.steps[0].starts_with("Confirm with Touch ID or your login password"), "{:?}", progress.steps);
        assert!(progress.steps[1].contains("no fingerprint"), "{:?}", progress.steps);
        assert_eq!(progress.steps[2], "sandbox");
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
            daemon: Daemon::Up,
            ..Model::default()
        }
    }

    fn welcome() -> Model {
        Model {
            new_project: Some(NewProject::default()),
            daemon: Daemon::Up,
            ..Model::default()
        }
    }

    fn plain(model: &Model, width: usize, height: usize) -> String {
        compose(model, width, height).plain()
    }

    /// The screen's text with its rules and line breaks taken out, so a
    /// phrase broken across rows can still be found.
    fn flat(model: &Model, width: usize, height: usize) -> String {
        // Beside the rail, only the right side, so that the names in the
        // list do not land in the middle of a sentence.
        let skip = if model.projects.is_empty() {
            0
        } else {
            (width.max(MIN_WIDTH) / 5).clamp(18, 28) + 1
        };
        plain(model, width, height)
            .lines()
            .map(|line| line.chars().skip(skip).collect::<String>())
            .map(|line| line.replace(['│', '─', '┌', '┐', '└', '┘', '║', '█', '▒'], " "))
            .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn bar(m: &Model, width: usize) -> String {
        plain(m, width, 24).lines().last().unwrap().to_owned()
    }

    fn with_tab(tab: Tab) -> Model {
        Model { tab, ..model() }
    }

    #[test]
    fn keys_that_arrive_together_are_taken_one_at_a_time() {
        assert_eq!(first_key_len(b"\x1b[A\x1b[A"), 3);
        assert_eq!(first_key_len(b"\x1b[5~j"), 4);
        assert_eq!(first_key_len(b"\x1bOAj"), 3);
        assert_eq!(first_key_len(b"jk"), 1);
        assert_eq!(first_key_len(b"\x1b"), 1);
    }

    #[test]
    fn the_screen_is_exactly_the_size_it_was_given() {
        for m in [model(), welcome()] {
            for (width, height) in [(80, 24), (60, 14), (120, 36), (200, 60), (10, 2)] {
                let rows = render(&m, width, height);
                assert_eq!(rows.len(), height.max(MIN_HEIGHT), "{width}x{height}");
                for row in &rows {
                    let drawn: usize = row.iter().map(|s| s.text.chars().count()).sum();
                    assert_eq!(drawn, width.max(MIN_WIDTH), "{width}x{height}");
                }
            }
        }
    }

    // The welcome page.

    #[test]
    fn with_no_project_the_page_offers_the_two_ways_in() {
        let text = flat(&welcome(), 120, 36);
        for phrase in [
            "Bring a project into a sandbox",
            "Your keys and secrets stay on this computer.",
            "g Clone from Git",
            "the key never goes in",
            "f Add a folder",
            "git@github.com:org/repo.git",
            "new . does the same",
        ] {
            assert!(text.contains(phrase), "{phrase}: {text}");
        }
        let bar = bar(&welcome(), 120);
        for shown in [
            "g Git",
            "f Folder",
            "↵ Continue",
            "? Help",
            "● daemon",
            "q Quit",
        ] {
            assert!(bar.contains(shown), "{shown}: {bar}");
        }
        assert!(!bar.contains("Back"), "nothing to go back to: {bar}");
        // No rail, and nothing that acts on a sandbox.
        assert!(!text.contains("PROJECTS"), "{text}");
        assert!(!bar.contains("Start") && !bar.contains("Stop"), "{bar}");
    }

    #[test]
    fn the_two_boxes_sit_side_by_side_when_there_is_room_and_stack_when_not() {
        let wide = plain(&welcome(), 120, 36);
        let row = wide.lines().find(|l| l.contains("Clone from Git")).unwrap();
        assert!(row.contains("Add a folder"), "{row}");
        let narrow = plain(&welcome(), 60, 24);
        let row = narrow
            .lines()
            .find(|l| l.contains("Clone from Git"))
            .unwrap();
        assert!(!row.contains("Add a folder"), "{narrow}");
        assert!(narrow.contains("Add a folder"), "{narrow}");
    }

    #[test]
    fn the_folder_this_was_started_in_is_offered() {
        let mut m = welcome();
        let new = m.new_project.as_mut().unwrap();
        new.offered = Some("/home/ada/code/billing-api".to_owned());
        new.chosen = Choice::Folder;
        let text = plain(&m, 120, 36);
        assert!(text.contains("/home/ada/code/billing-api"), "{text}");
        // The chosen box is the one marked.
        let rows = render(&m, 120, 36);
        let marked: String = rows
            .iter()
            .flatten()
            .filter(|s| s.style == Style::TabActive)
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(marked.trim(), "f");
    }

    #[test]
    fn typing_shows_the_line_with_a_cursor_and_only_enter_and_esc_in_the_bar() {
        let mut m = welcome();
        let new = m.new_project.as_mut().unwrap();
        new.typing = true;
        new.remote = "git@github.com:acme/demo_shop.git".to_owned();
        assert!(plain(&m, 120, 36).contains("git@github.com:acme/demo_shop.git_"));
        let line = bar(&m, 120);
        assert!(
            line.contains("↵ Fetch") && line.contains("Esc Cancel"),
            "{line}"
        );
        assert!(!line.contains("Quit"), "q types a letter here: {line}");
        m.new_project.as_mut().unwrap().chosen = Choice::Folder;
        assert!(bar(&m, 120).contains("↵ Plan"));
    }

    /// What is typed is drawn escaped, and a long line keeps its end in view,
    /// where the cursor is.
    #[test]
    fn a_typed_line_is_escaped_and_keeps_its_end_in_view() {
        let mut m = welcome();
        let new = m.new_project.as_mut().unwrap();
        new.chosen = Choice::Folder;
        new.typing = true;
        new.folder = format!("/{}/end\u{202e}x", "a".repeat(300));
        let text = plain(&m, 80, 24);
        assert!(text.contains("end"), "{text}");
        assert!(!text.contains('\u{202e}'), "{text}");
        for line in text.lines() {
            assert_eq!(line.chars().count(), 80);
        }
    }

    // Projects.

    #[test]
    fn the_rail_lists_new_project_then_each_project_with_its_state() {
        let text = plain(&model(), 120, 36);
        assert!(text.contains("+ New project"), "{text}");
        assert!(text.contains("PROJECTS"), "{text}");
        assert!(text.contains("● demo-shop-ab12cd34"), "{text}");
        assert!(text.contains("○ notes-ff00ff00"), "{text}");
        // The project's name, state and folder head the right side.
        let header = text.lines().find(|l| l.contains("running")).unwrap();
        assert!(
            header.contains("demo-shop-ab12cd34") && header.contains("/p/demo_shop"),
            "{header}"
        );
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
    fn every_tab_is_drawn_with_its_letter_and_the_one_showing_is_filled() {
        for tab in Tab::ALL {
            let m = with_tab(tab);
            let text = plain(&m, 120, 36);
            for other in Tab::ALL {
                assert!(
                    text.contains(&format!("{} {}", other.key(), other.label())),
                    "{text}"
                );
            }
            let rows = render(&m, 120, 36);
            let filled: String = rows
                .iter()
                .flatten()
                .filter(|s| s.style == Style::TabActive)
                .map(|s| s.text.as_str())
                .collect();
            assert_eq!(filled.trim(), format!("{} {}", tab.key(), tab.label()));
        }
    }

    #[test]
    fn on_a_narrow_screen_the_tabs_not_showing_keep_their_letter() {
        let mut m = with_tab(Tab::Logs);
        m.overview = Some(Overview {
            blocked_hosts: Some(3),
            ..Overview::default()
        });
        let text = plain(&m, MIN_WIDTH, MIN_HEIGHT);
        let row = text.lines().nth(2).unwrap();
        assert!(row.contains("l Logs"), "{row}");
        assert!(row.contains("n 3"), "{row}");
        for line in text.lines() {
            assert_eq!(line.chars().count(), MIN_WIDTH);
        }
    }

    #[test]
    fn network_counts_blocked_hostnames_and_changes_counts_changed_files() {
        let mut m = model();
        m.overview = Some(Overview {
            blocked_hosts: Some(3),
            ..Overview::default()
        });
        m.review.project = "/p/demo_shop".to_owned();
        m.review.files = Some(12);
        m.review.flags = 1;
        let text = plain(&m, 120, 36);
        assert!(text.contains("n Network 3"), "{text}");
        assert!(text.contains("c Changes 12"), "{text}");
        let rows = render(&m, 120, 36);
        assert!(rows
            .iter()
            .flatten()
            .any(|s| s.style == Style::WindowAlert && s.text == "3"));
        // Another project's comparison is not counted.
        m.selected = 1;
        assert!(!plain(&m, 120, 36).contains("Changes 12"));
        // No count is never drawn as zero.
        m.overview = Some(Overview::default());
        assert!(!plain(&m, 120, 36).contains("Network 0"));
    }

    /// The warning is in the top bar on every tab and every size, whatever
    /// the line above the bottom bar is saying.
    #[test]
    fn the_warning_is_always_in_the_top_bar_and_messages_have_their_own_line() {
        for (width, height) in [(MIN_WIDTH, MIN_HEIGHT), (80, 24), (200, 60)] {
            for tab in Tab::ALL {
                let mut m = with_tab(tab);
                m.message = Some("finished tidying".to_owned());
                let text = plain(&m, width, height);
                let top = text.lines().next().unwrap();
                assert!(top.contains("Early build"), "{width}: {top}");
                assert!(top.contains(brand::CLI_BIN), "{width}: {top}");
                assert!(
                    text.contains("finished tidying"),
                    "{width}x{height}: {text}"
                );
            }
            let text = plain(&welcome(), width, height);
            assert!(text.lines().next().unwrap().contains("Early build"));
        }
        let wide = plain(&model(), 120, 24);
        assert!(wide
            .lines()
            .next()
            .unwrap()
            .contains("Do not rely on it for untrusted code."));
        let rows = render(&model(), 80, 24);
        assert!(rows[0]
            .iter()
            .any(|s| s.style == Style::MenuAlert && s.text.contains("Early build")));
    }

    #[test]
    fn the_message_line_shows_what_is_happening_over_what_happened() {
        let mut m = model();
        m.message = Some("finished tidying".to_owned());
        assert!(plain(&m, 80, 24).contains("finished tidying"));
        m.busy = Some("working away".to_owned());
        let text = plain(&m, 80, 24);
        assert!(text.contains("working away"), "{text}");
        assert!(!text.contains("finished tidying"), "{text}");
        let quiet = plain(&model(), 80, 24);
        assert_eq!(quiet.matches("Early build").count(), 1, "{quiet}");
    }

    /// Help is longer than a line, and is shown whole above the bar rather
    /// than cut off.
    #[test]
    fn help_is_shown_whole_above_the_bar() {
        for m in [model(), welcome()] {
            let mut m = m;
            m.message = Some(help_for(&m).to_owned());
            for (width, height) in [(80, 24), (120, 36)] {
                let text = plain(&m, width, height);
                let joined = text
                    .lines()
                    .rev()
                    .skip(1)
                    .take(NOTE_LINES)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .map(|l| l.trim_start_matches([' ', '›']).trim_end())
                    .collect::<Vec<_>>()
                    .join(" ");
                let last_word = help_for(&m).split_whitespace().last().unwrap();
                assert!(joined.contains(last_word), "{width}: {joined}");
            }
        }
    }

    #[test]
    fn the_bar_says_whether_the_daemon_answered() {
        let mut m = model();
        assert!(bar(&m, 100).contains("● daemon"));
        m.daemon = Daemon::Down;
        assert!(bar(&m, 100).contains("daemon off"));
        let rows = render(&m, 100, 24);
        assert!(rows[23].iter().any(|s| s.style == Style::StatusAlert));
        m.daemon = Daemon::Unknown;
        assert!(bar(&m, 100).contains("○ daemon"));
    }

    /// A name from a project could otherwise push the page apart.
    #[test]
    fn a_very_long_name_cannot_break_the_page() {
        let mut m = model();
        m.projects[0].name = "n".repeat(500);
        m.projects[0].path = "p".repeat(500);
        for (width, height) in [(MIN_WIDTH, MIN_HEIGHT), (80, 24), (120, 36)] {
            let text = plain(&m, width, height);
            for line in text.lines() {
                assert_eq!(line.chars().count(), width);
            }
            assert!(text.contains("running"), "the state survived: {text}");
        }
    }

    #[test]
    fn a_long_list_of_projects_scrolls_and_keeps_the_choice_in_view() {
        let mut m = model();
        m.projects = (0..60)
            .map(|n| Row {
                name: format!("project-{n:02}"),
                path: format!("/p/{n}"),
                state: RowState::None,
            })
            .collect();
        m.selected = 59;
        let text = plain(&m, 80, 24);
        assert!(text.contains("project-59"), "{text}");
        assert!(!text.contains("project-00"), "{text}");
        assert!(text.contains('▲') && text.contains('▼'), "{text}");
    }

    // New project, beside the rail.

    #[test]
    fn new_project_shows_the_same_two_ways_beside_the_list() {
        let mut m = model();
        m.new_project = Some(NewProject::default());
        let text = flat(&m, 120, 36);
        assert!(
            text.contains("Bring another project into a sandbox"),
            "{text}"
        );
        assert!(
            text.contains("Clone from Git") && text.contains("Add a folder"),
            "{text}"
        );
        assert!(
            plain(&m, 120, 36).contains("demo-shop-ab12cd34"),
            "the list stays"
        );
        assert!(!text.contains("o Overview"), "{text}");
        let line = bar(&m, 120);
        assert!(line.contains("Esc Back"), "{line}");
        // New project is the row highlighted now.
        let rows = render(&m, 120, 36);
        let highlighted: String = rows
            .iter()
            .flatten()
            .filter(|s| s.style == Style::Selected)
            .map(|s| s.text.as_str())
            .collect();
        assert!(highlighted.contains("New project"), "{highlighted}");
        assert!(!highlighted.contains("demo-shop"), "{highlighted}");
        for (width, height) in [(MIN_WIDTH, MIN_HEIGHT), (80, 24)] {
            for line in plain(&m, width, height).lines() {
                assert_eq!(line.chars().count(), width);
            }
        }
    }

    // Overview.

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
                Entry::new("n to see them", "", Style::Window),
            ],
            blocked_alert: true,
            blocked_hosts: Some(3),
        });
        m
    }

    #[test]
    fn the_overview_has_a_box_for_each_part() {
        let m = overview_model();
        let text = plain(&m, 120, 36);
        for phrase in [
            "─ SERVICES ",
            "─ PORTS ",
            "─ RESOURCES ",
            "─ BLOCKED ",
            "127.0.0.1:3000",
            "postgres",
            "healthy",
            "6.0 GB",
            "6 refused lookups",
            "of 3 hostnames",
            "n to see them",
            "/p/demo_shop",
            "these came from inside the sandbox, so they can be wrong",
        ] {
            assert!(text.contains(phrase), "{phrase}: {text}");
        }
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
        overview.blocked = vec![Entry::new(
            "No hostnames recorded so far",
            "",
            Style::Window,
        )];
        let rows = render(&quiet, 100, 30);
        assert!(!rows.iter().flatten().any(|s| s.style == Style::FrameAlert));
    }

    /// On the smallest screen the boxes stack and the least needed go, but
    /// what was blocked always stays, and the page holds.
    #[test]
    fn the_overview_degrades_on_a_small_screen() {
        let m = overview_model();
        let text = plain(&m, MIN_WIDTH, MIN_HEIGHT);
        assert!(text.contains("BLOCKED"), "{text}");
        assert!(text.contains("6 refused"), "{text}");
        for line in text.lines() {
            assert_eq!(line.chars().count(), MIN_WIDTH);
        }
        // Services, when they fit, always bring the caveat with them.
        for height in MIN_HEIGHT..40 {
            let text = flat(&m, MIN_WIDTH, height);
            if text.contains("SERVICES") {
                assert!(text.contains("these came from inside"), "{height}: {text}");
            }
        }
        assert!(plain(&m, MIN_WIDTH, 30).contains("─ SERVICES"));
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
            assert!(text.contains('┘'));
        }
    }

    #[test]
    fn what_stops_a_start_is_shown_in_place_of_the_boxes() {
        let mut m = overview_model();
        m.detail = vec![Line::styled("needs a Compose file", Style::WindowAlert)];
        let text = plain(&m, 100, 30);
        assert!(text.contains("needs a Compose file"), "{text}");
        assert!(!text.contains("─ SERVICES"), "{text}");
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
        assert!(!text.contains("─ BLOCKED"), "{text}");
    }

    // Changes.

    #[test]
    fn the_changes_tab_says_how_to_compare_rather_than_looking_empty() {
        let text = flat(&with_tab(Tab::Changes), 80, 24);
        assert!(text.contains("Press r to compare"), "{text}");
        assert!(text.contains("p pulls and P pushes"), "{text}");
    }

    fn review_model() -> Model {
        let mut m = with_tab(Tab::Changes);
        m.review.project = "/p/demo_shop".to_owned();
        m.review.lines = vec![
            Line::plain(
                "Compared with the snapshot this sandbox started from (commit 1f2a12002068).",
            ),
            Line::styled(
                "FLAG  .claude/settings.json  adds a hook for an AI coding tool",
                Style::WindowAlert,
            ),
            Line::plain("1 file changed:"),
            Line::plain("  added     .claude/settings.json"),
        ];
        m
    }

    #[test]
    fn the_changes_tab_shows_flags_in_alert_colour() {
        let m = review_model();
        let text = plain(&m, 100, 30);
        assert!(text.contains("FLAG  .claude/settings.json"), "{text}");
        let row = text.lines().position(|l| l.contains("FLAG  ")).unwrap();
        let spans = &render(&m, 100, 30)[row];
        assert!(
            spans
                .iter()
                .any(|s| s.text.contains("FLAG") && s.style == Style::WindowAlert),
            "{spans:?}"
        );
    }

    #[test]
    fn the_changes_tab_scrolls_and_another_projects_result_is_not_shown() {
        let mut m = review_model();
        m.review.scroll_by(2);
        let text = plain(&m, 100, 30);
        assert!(!text.contains("FLAG  .claude"), "{text}");
        assert!(text.contains("1 file changed"), "{text}");
        m.review.scroll_by(100);
        assert_eq!(m.review.scroll, 3);
        m.review.scroll_by(-100);
        assert_eq!(m.review.scroll, 0);
        m.selected = 1;
        let text = plain(&m, 100, 30);
        assert!(!text.contains("FLAG"), "{text}");
        assert!(text.contains("Press r to compare"), "{text}");
    }

    // Secrets.

    fn secrets_model() -> Model {
        let mut m = with_tab(Tab::Secrets);
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
    fn the_secrets_tab_says_where_the_secrets_could_be_sent() {
        let mut m = secrets_model();
        m.secrets.reach = proto::exposure_lines(
            &[proto::HeldSecret {
                name: "API_KEY".into(),
                services: vec!["api".into()],
            }],
            Some(&["storage.googleapis.com".to_owned()]),
            true,
        );
        let text = flat(&m, 120, 36);
        assert!(text.contains("API_KEY is given to api"), "{text}");
        assert!(text.contains("storage.googleapis.com"), "{text}");
    }

    #[test]
    fn the_secrets_tab_lists_names_and_where_they_go_and_never_a_value() {
        let text = plain(&secrets_model(), 120, 30);
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

    #[test]
    fn empty_names_stand_out_and_the_panel_says_how_to_fill_them() {
        let mut m = secrets_model();
        let text = plain(&m, 120, 30);
        assert!(!text.contains("secrets fill"), "{text}");
        m.secrets.rows.push(SecretRow {
            name: "STRIPE_KEY".to_owned(),
            source: "empty".to_owned(),
            goes_to: "not set yet".to_owned(),
        });
        let text = plain(&m, 120, 30);
        assert!(text.contains("1 empty, with no value yet"), "{text}");
        assert!(text.contains("secrets fill"), "{text}");
        assert!(text.contains("Values are never shown"), "{text}");
    }

    /// Whatever the number of secrets and the size of the screen, the words
    /// saying values are never shown stay, and the page holds.
    #[test]
    fn the_secrets_tab_keeps_its_warning_however_many_there_are() {
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
            assert!(
                text.contains("Values are never shown"),
                "{width}x{height}: {text}"
            );
            for line in plain(&m, width, height).lines() {
                assert_eq!(line.chars().count(), width);
            }
        }
        let text = plain(&m, 120, 24);
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

    // Logs.

    fn logs_model() -> Model {
        let mut m = with_tab(Tab::Logs);
        m.logs.services = vec!["api".to_owned(), "web".to_owned()];
        m
    }

    #[test]
    fn the_logs_tab_shows_its_services_and_lines() {
        let mut m = logs_model();
        m.logs.lines = vec!["api-1  | hello there".to_owned()];
        let text = plain(&m, 80, 24);
        assert!(text.contains(" All  api  web "), "{text}");
        assert!(text.contains("api-1  | hello there"), "{text}");
        assert!(
            text.lines().last().unwrap().contains("←→ Service"),
            "{text}"
        );
        assert!(text.contains("so it can be wrong"), "{text}");
    }

    #[test]
    fn the_logs_tab_scrolls_back_and_stops_at_each_end() {
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

    #[test]
    fn the_logs_tab_keeps_the_newest_lines_when_they_do_not_all_fit() {
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
        assert!(
            text.contains("line-101") && text.contains("Following"),
            "{text}"
        );

        m.logs.live = Live::Ended;
        assert!(plain(&m, 80, 24).contains("Not following"));
    }

    /// The tag must survive the smallest screen and a long row of services
    /// that would otherwise push it off.
    #[test]
    fn the_paused_tag_fits_beside_many_services_on_a_small_screen() {
        let mut m = logs_model();
        m.projects[0].name = "n".repeat(300);
        m.logs.services = (0..30).map(|n| format!("service-{n:02}")).collect();
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
    fn a_long_log_line_cannot_break_the_page() {
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

    // Network.

    #[test]
    fn the_network_tab_always_says_what_it_cannot_show() {
        for (width, height) in [(80, 24), (60, 14), (200, 60)] {
            let mut m = with_tab(Tab::Network);
            let text = flat(&m, width, height);
            for phrase in [
                "hostnames only",
                "Raw IP, metadata and UDP",
                "doesn't mean nothing was tried",
            ] {
                assert!(text.contains(phrase), "{width}x{height}: {phrase}: {text}");
            }
            m.traffic.rows = (0..500).map(|n| format!("row-{n:03}")).collect();
            m.traffic.scroll_by(200);
            let text = flat(&m, width, height);
            assert!(
                text.contains("doesn't mean nothing was tried"),
                "{width}x{height}"
            );
        }
    }

    #[test]
    fn a_sandbox_that_records_nothing_says_so_even_on_the_smallest_screen() {
        let mut m = with_tab(Tab::Network);
        m.traffic.notes = vec![Line::styled(
            crate::blocked::not_recording_note(),
            Style::WindowAlert,
        )];
        for (width, height) in [(80, 24), (60, 14)] {
            let text = flat(&m, width, height);
            assert!(text.contains("Nothing is being recorded"), "{text}");
        }
        let text = flat(&m, 120, 36);
        assert!(text.contains("down --destroy"), "{text}");
        assert!(text.contains("deletes its data"), "{text}");
    }

    #[test]
    fn the_network_tab_keeps_the_newest_rows_and_scrolls_back_and_stops_at_each_end() {
        let mut m = with_tab(Tab::Network);
        m.traffic.rows = (0..500).map(|n| format!("row-{n:03}")).collect();
        let text = plain(&m, 120, 30);
        assert!(text.contains("row-499"), "{text}");
        assert!(!text.contains("row-000"), "{text}");
        assert!(text.contains("HOSTNAME"), "{text}");
        m.traffic.scroll_by(100);
        let text = plain(&m, 120, 30);
        assert!(text.contains("row-399"), "{text}");
        assert!(!text.contains("row-400"), "{text}");
        m.traffic.scroll_by(100_000);
        assert_eq!(m.traffic.scroll, 499);
        assert!(plain(&m, 120, 30).contains("row-000"));
        m.traffic.scroll_by(-100_000);
        assert_eq!(m.traffic.scroll, 0);
    }

    #[test]
    fn a_long_hostname_cannot_break_the_page() {
        let mut m = with_tab(Tab::Network);
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

    /// Every tab, full of data, at every size from the smallest up: nothing
    /// panics and every row is exactly the width.
    #[test]
    fn every_tab_draws_at_every_size() {
        let mut m = overview_model();
        m.logs.lines = (0..300).map(|n| format!("line {n}")).collect();
        m.logs.services = vec!["api".to_owned(), "web".to_owned()];
        m.traffic.rows = (0..300).map(|n| format!("row {n}")).collect();
        m.message = Some("something happened".to_owned());
        let mut screens: Vec<Model> = Tab::ALL
            .iter()
            .map(|tab| Model {
                tab: *tab,
                ..m.clone()
            })
            .collect();
        screens.push(welcome());
        screens.push(Model {
            new_project: Some(NewProject::default()),
            ..m.clone()
        });
        let mut asking = m.clone();
        asking.dialog = Some(question(80));
        screens.push(asking);
        for screen in &screens {
            for width in (MIN_WIDTH - 5..=130).step_by(7) {
                for height in (MIN_HEIGHT - 3..=45).step_by(3) {
                    let rows = render(screen, width, height);
                    assert_eq!(rows.len(), height.max(MIN_HEIGHT));
                    for row in rows {
                        let drawn: usize = row.iter().map(|s| s.text.chars().count()).sum();
                        assert_eq!(drawn, width.max(MIN_WIDTH), "{:?}", screen.tab);
                    }
                }
            }
        }
    }

    #[test]
    fn wrapping_breaks_at_spaces_and_keeps_every_word() {
        assert_eq!(wrap("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap("", 7), Vec::<String>::new());
        assert_eq!(wrap("longerthanwidth x", 4), ["longerthanwidth", "x"]);
        assert_eq!(
            wrap_indented("  one two three", 9),
            ["  one two", "  three"]
        );
        assert_eq!(wrap_indented("", 9), [""]);
        // In a plan, a word too long for the box is broken, never cut.
        assert_eq!(
            wrap_indented("  abcdefghij", 6),
            ["  abcd", "  efgh", "  ij"]
        );
    }

    // Questions.

    fn question(lines: usize) -> Dialog {
        Dialog::question(
            "Start demo-shop",
            (0..lines)
                .map(|n| Line::plain(format!("plan line {n:02}")))
                .collect(),
        )
    }

    #[test]
    fn a_question_is_a_box_over_the_rest_with_no_as_the_default() {
        let mut m = model();
        m.dialog = Some(question(3));
        let text = plain(&m, 90, 26);
        assert!(text.contains("╡ Start demo-shop ╞"), "{text}");
        assert!(text.contains("Yes") && text.contains("No"));
        let rows = render(&m, 90, 26);
        let marked: String = rows
            .iter()
            .flatten()
            .filter(|s| s.style == Style::Button || s.style == Style::ButtonHot)
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(marked.trim(), "No", "No is the button marked");
        let line = bar(&m, 90);
        assert!(line.contains("y Yes") && line.contains("n No"), "{line}");
        assert!(!line.contains("Quit"), "{line}");
    }

    /// A plan longer than the screen is read by scrolling, and Yes counts only
    /// once its last line has been drawn.
    #[test]
    fn yes_waits_until_the_whole_question_has_been_on_screen() {
        let mut dialog = question(60);
        dialog.note_seen(80, 24);
        assert!(!dialog.seen_all);
        let mut m = model();
        m.dialog = Some(dialog.clone());
        let text = plain(&m, 80, 24);
        assert!(text.contains("plan line 00"), "{text}");
        assert!(!text.contains("plan line 59"), "{text}");
        assert!(text.contains("more below"), "{text}");
        dialog.scroll_by(1000, 80, 24);
        dialog.note_seen(80, 24);
        assert!(dialog.seen_all);
        m.dialog = Some(dialog.clone());
        let text = plain(&m, 80, 24);
        assert!(text.contains("plan line 59"), "{text}");
        assert!(!text.contains("more below"), "{text}");
        // Scrolling back does not take it away.
        dialog.scroll_by(-1000, 80, 24);
        dialog.note_seen(80, 24);
        assert!(dialog.seen_all);
        // What a key said while it shows is not hidden under it.
        let mut m = model();
        m.dialog = Some(question(60));
        m.message = Some("Read all of it first".to_owned());
        for (width, height) in [(MIN_WIDTH, MIN_HEIGHT), (80, 24)] {
            assert!(plain(&m, width, height).contains("Read all of it first"));
        }
        // A short one is seen as soon as it is drawn.
        let mut short = question(3);
        short.note_seen(80, 24);
        assert!(short.seen_all);
    }

    #[test]
    fn a_long_line_in_a_question_is_wrapped_not_cut() {
        let mut m = model();
        m.dialog = Some(Dialog::question(
            "Start",
            vec![Line::plain(format!(
                "  while building: {}",
                ["a.example.com"; 20].join(", ")
            ))],
        ));
        let text = plain(&m, 80, 24);
        assert_eq!(text.matches("a.example.com").count(), 20, "{text}");
    }

    #[test]
    fn a_report_box_says_how_to_close_it() {
        let mut m = model();
        m.dialog = Some(Dialog::notice(
            "Pull done",
            vec![Line::plain("Already up to date.")],
            "Press any key",
        ));
        let text = plain(&m, 90, 26);
        assert!(
            text.contains("Press any key") && !text.contains(" Yes "),
            "{text}"
        );
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
        assert!(text.contains("✓ step 38: detail"), "{text}");
        assert!(!text.contains("step 00"), "old steps scroll away: {text}");
        // Keys do nothing while it runs, so the bar offers none.
        assert!(!bar(&m, 90).contains("Quit"));
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

    // The bar and the keys.

    /// Only keys that do something where you are, and the keys that act on
    /// a sandbox only on Overview, and only those its state allows.
    #[test]
    fn each_tab_shows_only_its_own_keys() {
        let mut m = model();
        let running = bar(&m, 120);
        for shown in [
            "↑↓ Project",
            "S Stop",
            "x Destroy",
            "e Shell",
            "r Refresh",
            "? Help",
            "q Quit",
        ] {
            assert!(running.contains(shown), "{shown}: {running}");
        }
        assert!(!running.contains("Start"), "it is running: {running}");
        assert!(!running.contains("Follow"), "{running}");

        m.selected = 1;
        let stopped = bar(&m, 120);
        assert!(
            stopped.contains("u Start") && stopped.contains("x Destroy"),
            "{stopped}"
        );
        for hidden in ["Stop", "Shell"] {
            assert!(!stopped.contains(hidden), "{hidden}: {stopped}");
        }
        m.projects[1].state = RowState::None;
        let none = bar(&m, 120);
        assert!(
            none.contains("u Start") && !none.contains("Destroy"),
            "{none}"
        );

        let logs = bar(&with_tab(Tab::Logs), 120);
        for shown in [
            "←→ Service",
            "↑↓ Scroll",
            "f Follow",
            "e Shell",
            "r Refresh",
        ] {
            assert!(logs.contains(shown), "{shown}: {logs}");
        }
        for hidden in ["Start", "Stop", "Destroy", "Pull"] {
            assert!(!logs.contains(hidden), "{hidden}: {logs}");
        }

        let network = bar(&with_tab(Tab::Network), 120);
        assert!(
            network.contains("↑↓ Scroll") && network.contains("r Refresh"),
            "{network}"
        );
        for hidden in ["Start", "Shell", "Service", "Follow"] {
            assert!(!network.contains(hidden), "{hidden}: {network}");
        }

        let changes = bar(&with_tab(Tab::Changes), 120);
        for shown in ["r Compare", "p Pull", "P Push"] {
            assert!(changes.contains(shown), "{shown}: {changes}");
        }
        assert!(!changes.contains("Stop"), "{changes}");
    }

    /// On the narrowest screen the bar still fits, keeps Help, says whether
    /// the daemon answered, and still ends with quitting.
    #[test]
    fn the_key_bar_fits_the_narrowest_screen_with_help_and_quit() {
        let mut screens: Vec<Model> = Tab::ALL.iter().map(|tab| with_tab(*tab)).collect();
        screens.push(welcome());
        for m in screens {
            let line = bar(&m, MIN_WIDTH);
            assert_eq!(line.chars().count(), MIN_WIDTH);
            assert!(line.contains("? Help"), "{line}");
            assert!(line.trim_end().ends_with("q Quit"), "{line}");
            assert!(line.contains("daemon"), "{line}");
        }
    }

    /// Help names the keys the bar shows, so the two cannot drift apart.
    #[test]
    fn help_mentions_what_the_bar_offers() {
        let mut screens: Vec<Model> = Tab::ALL.iter().map(|tab| with_tab(*tab)).collect();
        screens.push(welcome());
        let mut stopped = model();
        stopped.selected = 1;
        screens.push(stopped);
        for m in screens {
            let help = help_for(&m);
            for (key, _) in keys_for(&m) {
                let named = match key {
                    "←→" => "Left and right",
                    "↑↓" => "Up",
                    "↵" => "Enter",
                    "?" | "q" => continue,
                    other => other,
                };
                assert!(help.contains(named), "{:?}: {named}: {help}", m.tab);
            }
        }
    }

    #[test]
    fn tabs_cycle_both_ways() {
        let mut tab = Tab::Overview;
        for _ in 0..Tab::ALL.len() {
            tab = tab.next();
        }
        assert_eq!(tab, Tab::Overview);
        assert_eq!(Tab::Overview.previous(), Tab::Secrets);
        assert_eq!(Tab::Secrets.next(), Tab::Overview);
    }

    /// Each tab opens with the letter drawn on it, and that letter does
    /// nothing else. What acts on a sandbox has a key of its own.
    #[test]
    fn the_letters_on_the_tabs_open_them_and_never_act_on_a_sandbox() {
        for tab in Tab::ALL {
            assert_eq!(key(tab.key().to_string().as_bytes()), Key::Open(tab));
        }
        assert_eq!(key(b"u"), Key::Start);
        assert_eq!(key(b"\x1bOQ"), Key::Start);
        assert_eq!(key(b"S"), Key::Stop);
        assert_eq!(key(b"\x1bOR"), Key::Stop);
        assert_eq!(key(b"x"), Key::Destroy);
        assert_eq!(key(b"e"), Key::Shell);
        assert_eq!(key(b"p"), Key::Pull);
        assert_eq!(key(b"P"), Key::Push);
        assert_eq!(key(b"+"), Key::NewProject);
        assert_eq!(key(b"N"), Key::NewProject);
        assert_eq!(key(b"r"), Key::Refresh);
        assert_eq!(key(b"\x1b[15~"), Key::Refresh);
        assert_eq!(key(b"\t"), Key::NextTab);
        assert_eq!(key(b"\x1b[Z"), Key::PreviousTab);
        assert_eq!(key(b"q"), Key::Quit);
        assert_eq!(key(b"\x1b[21~"), Key::Quit);
        // Raw mode stops the shell ending this, so the interface has to.
        assert_eq!(key(b"\x03"), Key::Quit);
        assert_eq!(key(b"?"), Key::Help);
        assert_eq!(key(b"\x1b[A"), Key::Up);
        assert_eq!(key(b"Z"), Key::Other);
        // The old number keys do nothing now, rather than something else.
        for old in [&b"1"[..], b"2", b"3", b"5", b"6", b"0"] {
            assert_eq!(key(old), Key::Other, "{old:?}");
        }
        assert_eq!(key(b"\r"), Key::Other);
    }

    #[test]
    fn the_side_arrows_choose_a_service_and_the_page_keys_are_known() {
        assert_eq!(key(b"\x1b[D"), Key::PreviousService);
        assert_eq!(key(b"\x1b[C"), Key::NextService);
        assert_eq!(key(b"["), Key::PreviousService);
        assert_eq!(key(b"]"), Key::NextService);
        assert_eq!(key(b"\x1b[5~"), Key::PageUp);
        assert_eq!(key(b"\x1b[6~"), Key::PageDown);
        for bytes in [&b"f"[..], b"\x1b[F", b"\x1bOF", b"\x1b[4~"] {
            assert_eq!(key(bytes), Key::Follow, "{bytes:?}");
        }
    }

    #[test]
    fn the_new_project_page_has_its_own_keys() {
        assert_eq!(choice_key(b"g"), ChoiceKey::Git);
        assert_eq!(choice_key(b"f"), ChoiceKey::Folder);
        assert_eq!(choice_key(b"\r"), ChoiceKey::Enter);
        assert_eq!(choice_key(b"\x1b"), ChoiceKey::Back);
        assert_eq!(choice_key(b"\x1b[D"), ChoiceKey::Left);
        assert_eq!(choice_key(b"\x1b[C"), ChoiceKey::Right);
        assert_eq!(choice_key(b"q"), ChoiceKey::Quit);
        // Nothing here starts, stops or destroys anything.
        for other in [&b"u"[..], b"S", b"x", b"y"] {
            assert_eq!(choice_key(other), ChoiceKey::Other, "{other:?}");
        }
    }

    /// Only y agrees. Enter, the default, and everything that means leave,
    /// say no.
    #[test]
    fn only_y_answers_yes_and_enter_means_no() {
        assert_eq!(answer(b"y"), Answer::Yes);
        assert_eq!(answer(b"Y"), Answer::Yes);
        for no in [&b"n"[..], b"N", b"\x1b", b"\r", b"\n", b"q", b"\x03"] {
            assert_eq!(answer(no), Answer::No, "{no:?}");
        }
        for other in [&b"u"[..], b" ", b"2", b"x"] {
            assert_eq!(answer(other), Answer::Other, "{other:?}");
        }
        assert_eq!(answer(b"\x1b[B"), Answer::Down);
    }

    #[test]
    fn what_a_state_allows() {
        assert!(!RowState::Running.offers().start);
        assert!(RowState::Running.offers().stop && RowState::Running.offers().shell);
        assert!(RowState::Stopped.offers().start && !RowState::Stopped.offers().stop);
        assert!(!RowState::None.offers().destroy);
        let unknown = RowState::Unknown.offers();
        assert!(unknown.start && unknown.stop && unknown.destroy && unknown.shell);
        assert!(!Model::default().offers().start);
    }

    /// Prints the main screens, for a person to look at:
    /// `cargo test -p cli print_screens -- --ignored --nocapture`.
    #[test]
    #[ignore = "prints screens for a person to read"]
    fn print_screens() {
        let mut offered = welcome();
        let new = offered.new_project.as_mut().unwrap();
        new.offered = Some("/home/ada/code/billing-api".to_owned());
        new.chosen = Choice::Folder;
        let mut typing = model();
        typing.new_project = Some(NewProject {
            typing: true,
            remote: "git@github.com:acme/demo_shop.git".to_owned(),
            ..NewProject::default()
        });
        let mut logs = logs_model();
        logs.logs.live = Live::Following;
        logs.logs.lines = (0..40)
            .map(|n| format!("api-1  | GET /health 200 {n}ms"))
            .collect();
        for (name, m) in [
            ("welcome", offered),
            ("overview", overview_model()),
            ("new project, typing", typing),
            ("logs", logs),
            ("changes", review_model()),
        ] {
            for (w, h) in [(120, 36), (80, 24)] {
                println!("--- {name} {w}x{h}\n{}", plain(&m, w, h));
            }
        }
    }
}
