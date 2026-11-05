//! What the interface looks like, worked out as plain text.
//!
//! This module touches no terminal. It takes a model and a size and returns
//! the lines to draw, which is what makes the layout testable. Colour is
//! added afterwards, so a width calculation never counts an escape sequence.
//!
//! Every string here that came from a project or a sandbox arrives already
//! escaped, because it came through the daemon. It is cut to the width, which
//! is the only other thing needed to keep a hostile name from breaking the
//! frame.

use crate::ui::Tone;

/// One rendered line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub tone: Tone,
}

impl Line {
    fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: Tone::Dim,
        }
    }

    fn toned(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }
}

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
            RowState::None => "-",
            RowState::Unknown => "?",
        }
    }

    fn tone(self) -> Tone {
        match self {
            RowState::Running => Tone::Good,
            RowState::Stopped => Tone::Warn,
            RowState::None => Tone::Dim,
            RowState::Unknown => Tone::Bad,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub path: String,
    pub state: RowState,
}

#[derive(Debug, Clone, Default)]
pub struct Model {
    pub projects: Vec<Row>,
    pub selected: usize,
    pub detail: Vec<Line>,
    /// Shown on the bottom line. Cleared by the next action.
    pub message: Option<String>,
    /// Set while a request is in flight.
    pub busy: Option<String>,
    pub pane: PaneState,
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
        Pane::Projects => &[],
        Pane::Traffic => &[
            "Outbound requests are not recorded on the host yet.",
            "",
            "There is no gateway and no proxy, so nothing decides or logs a",
            "request on real traffic. An empty panel here does not mean",
            "nothing was blocked: it means nothing is watching.",
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
        Pane::Logs => &[
            "Reading a sandbox's logs is not built.",
            "",
            "When it is, remember that logs come from inside the sandbox, so",
            "they can be wrong. Only the host's own records cannot be edited",
            "from in there.",
        ],
    }
}

/// Builds exactly `height` lines, each at most `width` characters.
pub fn render(model: &Model, width: usize, height: usize) -> Vec<Line> {
    let width = width.max(40);
    let height = height.max(10);
    let mut lines = Vec::with_capacity(height);

    lines.push(Line::toned(
        frame_top(&format!(" {} ", brand::PRODUCT_NAME), width),
        Tone::Strong,
    ));
    lines.push(Line::toned(bar(&tabs(model.pane.0), width), Tone::Strong));
    lines.push(Line::plain(divider(width)));

    // Two lines of frame at the bottom, plus the message line.
    let body = height.saturating_sub(lines.len() + 3);
    let mut used = 0;
    for line in body_lines(model, width, body) {
        lines.push(line);
        used += 1;
    }
    for _ in used..body {
        lines.push(Line::plain(bar("", width)));
    }

    lines.push(Line::plain(divider(width)));
    let hint = match model.pane.0 {
        Pane::Projects => "up/down move  u start  s stop  x destroy  r refresh  tab panel  q quit",
        _ => "tab panel  q quit",
    };
    lines.push(Line::plain(bar(&cut(hint, width.saturating_sub(4)), width)));
    let note = match (&model.busy, &model.message) {
        (Some(busy), _) => Line::toned(bar(&cut(busy, width - 4), width), Tone::Warn),
        (None, Some(message)) => Line::toned(bar(&cut(message, width - 4), width), Tone::Warn),
        (None, None) => Line::toned(
            bar(
                &cut(
                    "Nothing here has run against a real VM. It is not protecting anything.",
                    width - 4,
                ),
                width,
            ),
            Tone::Bad,
        ),
    };
    lines.push(note);
    lines.truncate(height);
    lines
}

fn body_lines(model: &Model, width: usize, height: usize) -> Vec<Line> {
    if model.pane.0 != Pane::Projects {
        return not_built(model.pane.0)
            .iter()
            .take(height)
            .map(|text| Line::plain(bar(&cut(text, width - 4), width)))
            .collect();
    }
    if model.projects.is_empty() {
        return vec![
            Line::plain(bar(&cut("No project has been started here.", width - 4), width)),
            Line::plain(bar("", width)),
            Line::plain(bar(
                &cut("Run the up command in a project folder first.", width - 4),
                width,
            )),
        ];
    }

    // The list takes the top half, the detail the rest.
    let list_height = (height / 2).clamp(1, model.projects.len().max(1)).min(height);
    let mut lines = Vec::new();
    let first = model
        .selected
        .saturating_sub(list_height.saturating_sub(1));
    for (index, row) in model
        .projects
        .iter()
        .enumerate()
        .skip(first)
        .take(list_height)
    {
        let marker = if index == model.selected { ">" } else { " " };
        let state = row.state.word();
        let room = width.saturating_sub(14 + state.len());
        let text = format!("{marker} {:<width$}  {state}", cut(&row.name, room), width = room);
        lines.push(Line::toned(
            bar(&cut(&text, width - 4), width),
            if index == model.selected {
                Tone::Strong
            } else {
                row.state.tone()
            },
        ));
    }
    if lines.len() < height {
        lines.push(Line::plain(divider(width)));
    }
    for line in model.detail.iter().take(height.saturating_sub(lines.len())) {
        lines.push(Line::toned(
            bar(&cut(&line.text, width - 4), width),
            line.tone,
        ));
    }
    lines
}

fn tabs(current: Pane) -> String {
    Pane::ALL
        .iter()
        .map(|pane| {
            if *pane == current {
                format!("[{}]", pane.label())
            } else {
                format!(" {} ", pane.label())
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

fn frame_top(title: &str, width: usize) -> String {
    let title = cut(title, width.saturating_sub(4));
    let rest = width.saturating_sub(2 + title.chars().count());
    format!("┌{title}{}┐", "─".repeat(rest))
}

fn divider(width: usize) -> String {
    format!("├{}┤", "─".repeat(width.saturating_sub(2)))
}

fn bar(text: &str, width: usize) -> String {
    let inner = width.saturating_sub(4);
    let text = cut(text, inner);
    let pad = inner.saturating_sub(text.chars().count());
    format!("│ {text}{} │", " ".repeat(pad))
}

/// Cuts to a number of characters. Width is counted in characters, which is
/// wrong for double-width glyphs; a cut line is better than a broken frame.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    text.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}

/// A key the interface understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Tab,
    Quit,
    Refresh,
    Start,
    Stop,
    Destroy,
    Other,
}

/// Reads one key from bytes already taken from the terminal.
pub fn key(bytes: &[u8]) -> Key {
    match bytes {
        b"\x1b[A" | b"k" => Key::Up,
        b"\x1b[B" | b"j" => Key::Down,
        b"\t" => Key::Tab,
        b"q" | b"\x03" => Key::Quit,
        b"r" => Key::Refresh,
        b"u" => Key::Start,
        b"s" => Key::Stop,
        b"x" => Key::Destroy,
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
                    name: "demo-shop".to_owned(),
                    path: "/p/demo_shop".to_owned(),
                    state: RowState::Running,
                },
                Row {
                    name: "other".to_owned(),
                    path: "/p/other".to_owned(),
                    state: RowState::None,
                },
            ],
            ..Model::default()
        }
    }

    fn plain(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_screen_is_exactly_the_size_it_was_given() {
        for (width, height) in [(80, 24), (40, 10), (200, 60), (10, 2)] {
            let lines = render(&model(), width, height);
            let expected_height = height.max(10);
            assert_eq!(lines.len(), expected_height, "{width}x{height}");
            for line in &lines {
                assert!(
                    line.text.chars().count() <= width.max(40),
                    "{width}x{height}: {:?}",
                    line.text
                );
            }
        }
    }

    /// A name from a project could otherwise push the frame off the screen.
    #[test]
    fn a_very_long_name_cannot_break_the_frame() {
        let mut m = model();
        m.projects[0].name = "n".repeat(500);
        let lines = render(&m, 80, 24);
        for line in &lines {
            assert!(line.text.chars().count() <= 80);
            if line.text.starts_with('│') {
                assert!(line.text.ends_with('│'), "{:?}", line.text);
            }
        }
    }

    #[test]
    fn the_warning_shows_until_something_else_needs_the_line() {
        let lines = render(&model(), 80, 24);
        assert!(plain(&lines).contains("not protecting anything"));

        let mut m = model();
        m.message = Some("stopped".to_owned());
        assert!(plain(&render(&m, 80, 24)).contains("stopped"));

        m.busy = Some("starting".to_owned());
        let text = plain(&render(&m, 80, 24));
        assert!(text.contains("starting"));
        assert!(!text.contains("stopped"));
    }

    #[test]
    fn every_panel_that_is_not_built_says_so_rather_than_looking_empty() {
        // Pinned wording, because the whole point of these panels is that
        // they say what does not work instead of looking like nothing
        // happened.
        for (pane, phrase) in [
            (Pane::Traffic, "are not recorded on the host yet"),
            (Pane::Secrets, "nothing delivers them to a sandbox"),
            (Pane::Review, "is not built"),
            (Pane::Logs, "is not built"),
        ] {
            let mut m = model();
            m.pane = PaneState(pane);
            let text = plain(&render(&m, 80, 24));
            assert!(text.contains(phrase), "{}: {text}", pane.label());
        }
    }

    /// An empty traffic panel must not read as "nothing was blocked".
    #[test]
    fn the_traffic_panel_says_nothing_is_watching() {
        let mut m = model();
        m.pane = PaneState(Pane::Traffic);
        assert!(plain(&render(&m, 80, 24)).contains("nothing is watching"));
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
    fn the_keys_are_what_the_hint_line_promises() {
        assert_eq!(key(b"\x1b[A"), Key::Up);
        assert_eq!(key(b"k"), Key::Up);
        assert_eq!(key(b"\x1b[B"), Key::Down);
        assert_eq!(key(b"j"), Key::Down);
        assert_eq!(key(b"\t"), Key::Tab);
        assert_eq!(key(b"q"), Key::Quit);
        // Ctrl-C ends it too, because raw mode stops the shell from doing it.
        assert_eq!(key(b"\x03"), Key::Quit);
        assert_eq!(key(b"u"), Key::Start);
        assert_eq!(key(b"s"), Key::Stop);
        assert_eq!(key(b"x"), Key::Destroy);
        assert_eq!(key(b"Z"), Key::Other);
        let hint = plain(&render(&model(), 120, 24));
        for promised in ["u start", "s stop", "x destroy", "r refresh", "q quit"] {
            assert!(hint.contains(promised), "{promised} missing from {hint}");
        }
    }

    #[test]
    fn the_selected_project_is_marked() {
        let mut m = model();
        m.selected = 1;
        let text = plain(&render(&m, 80, 24));
        assert!(text.contains("> other"), "{text}");
    }
}
