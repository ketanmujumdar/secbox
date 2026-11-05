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
    }

    /// Where the choice is along All and the services, with All first.
    fn chosen_index(&self) -> usize {
        self.chosen
            .as_ref()
            .and_then(|name| self.services.iter().position(|s| s == name))
            .map_or(0, |index| index + 1)
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
        Pane::Projects | Pane::Logs => &[],
        Pane::Traffic => &[
            "Outbound requests are not recorded on the host yet.",
            "",
            "The engine blocks hosts that are not on the allowlist, but it",
            "does not report them here. An empty panel does not mean nothing",
            "was blocked: it means nothing is being shown yet.",
        ],
        Pane::Secrets => &[
            "Secrets are stored, but nothing delivers them to a sandbox.",
            "",
            "Use the secrets command to add and assign them. A secret held by",
            "the proxy also needs the gateway to terminate TLS, which is not",
            "settled, so a plan carrying one is refused.",
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
    screen.text_cut(x + 1, y, &row.name, w - 2, Style::WindowDim);
    service_bar(panel, screen, x + 1, y + 1, w - 2);
    screen.text_cut(
        x + 1,
        y + 2,
        "[ ] choose  5 refresh. From inside the sandbox, so they can be wrong.",
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
    let first = panel.lines.len().saturating_sub(fits);
    for line in &panel.lines[first..] {
        screen.text_cut(x + 1, at, line, w - 2, Style::Window);
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
    Yes,
    No,
    Other,
}

/// Reads one keystroke. Function keys work as the bar along the bottom says,
/// and the letters work too, because not every terminal sends every one.
pub fn key(bytes: &[u8]) -> Key {
    match bytes {
        b"\x1b[A" | b"k" => Key::Up,
        b"\x1b[B" | b"j" => Key::Down,
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
        b"[" => Key::PreviousService,
        b"]" => Key::NextService,
        b"?" | b"1" | b"\x1bOP" => Key::Help,
        b"y" | b"Y" => Key::Yes,
        b"n" | b"N" | b"\x1b" => Key::No,
        _ => Key::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            (Pane::Traffic, "are not recorded on the host yet"),
            (Pane::Secrets, "nothing delivers them to a sandbox"),
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
        assert!(text.contains("[ ] choose"), "{text}");
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

    #[test]
    fn the_traffic_panel_says_an_empty_list_is_not_proof() {
        let mut m = model();
        m.pane = PaneState(Pane::Traffic);
        assert!(plain(&m, 80, 24).contains("nothing is being shown yet"));
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
        assert_eq!(key(b"Z"), Key::Other);
    }
}
