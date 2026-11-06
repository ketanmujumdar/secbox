//! A grid of coloured characters, drawn into and then turned into lines.
//!
//! Drawing into a grid rather than building strings is what makes boxes
//! and dialogs that overlap each other simple, and it means the
//! width of a row is always exactly the width of the screen. Nothing here
//! writes an escape sequence; styles stay as styles until the very end.

use super::style::Style;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cell {
    ch: char,
    style: Style,
}

/// A rectangle on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    pub fn new(x: usize, y: usize, w: usize, h: usize) -> Self {
        Self { x, y, w, h }
    }
}

/// A run of characters sharing one style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

pub struct Screen {
    width: usize,
    height: usize,
    cells: Vec<Cell>,
}

impl Screen {
    pub fn new(width: usize, height: usize, fill: char, style: Style) -> Self {
        Self {
            width,
            height,
            cells: vec![Cell { ch: fill, style }; width * height],
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn put(&mut self, x: usize, y: usize, ch: char, style: Style) {
        if x < self.width && y < self.height {
            self.cells[y * self.width + x] = Cell { ch, style };
        }
    }

    /// Writes text from a point, stopping at the edge rather than wrapping.
    /// Returns how many characters fitted.
    pub fn text(&mut self, x: usize, y: usize, text: &str, style: Style) -> usize {
        let mut written = 0;
        for (offset, ch) in text.chars().enumerate() {
            if x + offset >= self.width {
                break;
            }
            self.put(x + offset, y, ch, style);
            written += 1;
        }
        written
    }

    /// Writes text cut to `room`, with a marker when it did not fit.
    pub fn text_cut(&mut self, x: usize, y: usize, text: &str, room: usize, style: Style) {
        self.text(x, y, &cut(text, room), style);
    }

    pub fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, ch: char, style: Style) {
        for row in y..(y + h).min(self.height) {
            for col in x..(x + w).min(self.width) {
                self.put(col, row, ch, style);
            }
        }
    }

    /// A double-ruled box, for what sits over everything else, with its
    /// title set into the top rule.
    pub fn frame(&mut self, at: Rect, style: Style, title: &str, title_style: Style) {
        let Rect { x, y, w, h } = at;
        if w < 2 || h < 2 {
            return;
        }
        let (right, bottom) = (x + w - 1, y + h - 1);
        self.put(x, y, '╔', style);
        self.put(right, y, '╗', style);
        self.put(x, bottom, '╚', style);
        self.put(right, bottom, '╝', style);
        for col in x + 1..right {
            self.put(col, y, '═', style);
            self.put(col, bottom, '═', style);
        }
        for row in y + 1..bottom {
            self.put(x, row, '║', style);
            self.put(right, row, '║', style);
        }
        if !title.is_empty() && w > 8 {
            let label = cut(title, w - 8);
            let shown = label.chars().count() + 4;
            let start = x + (w.saturating_sub(shown)) / 2;
            self.put(start, y, '╡', style);
            self.put(start + 1, y, ' ', title_style);
            self.text(start + 2, y, &label, title_style);
            self.put(start + 2 + label.chars().count(), y, ' ', title_style);
            self.put(start + 3 + label.chars().count(), y, '╞', style);
        }
    }

    /// A single-ruled box, with its title near the left of the top rule,
    /// which is how related things are grouped on the page.
    pub fn group_box(&mut self, at: Rect, style: Style, title: &str, title_style: Style) {
        let Rect { x, y, w, h } = at;
        if w < 2 || h < 2 {
            return;
        }
        let (right, bottom) = (x + w - 1, y + h - 1);
        self.put(x, y, '┌', style);
        self.put(right, y, '┐', style);
        self.put(x, bottom, '└', style);
        self.put(right, bottom, '┘', style);
        for col in x + 1..right {
            self.put(col, y, '─', style);
            self.put(col, bottom, '─', style);
        }
        for row in y + 1..bottom {
            self.put(x, row, '│', style);
            self.put(right, row, '│', style);
        }
        if !title.is_empty() && w > 6 {
            let label = format!(" {} ", cut(title, w - 6));
            self.text(x + 2, y, &label, title_style);
        }
    }

    /// A single-ruled line across part of the page.
    pub fn rule(&mut self, x: usize, y: usize, w: usize, style: Style) {
        for col in x..x + w {
            self.put(col, y, '─', style);
        }
    }

    /// A single-ruled line down part of the page.
    pub fn column_rule(&mut self, x: usize, y: usize, h: usize, style: Style) {
        for row in y..y + h {
            self.put(x, row, '│', style);
        }
    }

    /// A scrollbar down the right-hand edge of a list.
    pub fn scrollbar(
        &mut self,
        x: usize,
        y: usize,
        h: usize,
        first: usize,
        shown: usize,
        total: usize,
    ) {
        if h < 3 {
            return;
        }
        self.put(x, y, '▲', Style::ScrollThumb);
        self.put(x, y + h - 1, '▼', Style::ScrollThumb);
        let track = h - 2;
        for row in 1..=track {
            self.put(x, y + row, '▒', Style::Scrollbar);
        }
        if total == 0 || shown >= total {
            for row in 1..=track {
                self.put(x, y + row, '█', Style::ScrollThumb);
            }
            return;
        }
        let size = ((shown * track) / total).max(1);
        let span = total.saturating_sub(shown).max(1);
        let at = (first * track.saturating_sub(size)) / span;
        for row in 0..size {
            self.put(
                x,
                y + 1 + (at + row).min(track - 1),
                '█',
                Style::ScrollThumb,
            );
        }
    }

    /// Turns the grid into one list of runs per row.
    pub fn rows(&self) -> Vec<Vec<Span>> {
        let mut rows = Vec::with_capacity(self.height);
        for y in 0..self.height {
            let mut spans: Vec<Span> = Vec::new();
            for x in 0..self.width {
                let cell = self.cells[y * self.width + x];
                match spans.last_mut() {
                    Some(last) if last.style == cell.style => last.text.push(cell.ch),
                    _ => spans.push(Span {
                        text: cell.ch.to_string(),
                        style: cell.style,
                    }),
                }
            }
            rows.push(spans);
        }
        rows
    }

    /// The grid as plain text, which is what a test reads.
    #[cfg(test)]
    pub fn plain(&self) -> String {
        (0..self.height)
            .map(|y| {
                (0..self.width)
                    .map(|x| self.cells[y * self.width + x].ch)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Cuts to a number of characters. Width is counted in characters, which is
/// wrong for double-width glyphs; a cut line is better than a broken frame.
pub fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    text.chars().take(max - 1).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_is_always_exactly_the_width() {
        let mut screen = Screen::new(20, 4, ' ', Style::Window);
        screen.text(
            0,
            0,
            "a very long line that does not fit at all",
            Style::Window,
        );
        for row in screen.rows() {
            let width: usize = row.iter().map(|s| s.text.chars().count()).sum();
            assert_eq!(width, 20);
        }
        assert_eq!(screen.plain().lines().count(), 4);
    }

    #[test]
    fn writing_past_the_edge_stops_rather_than_wrapping() {
        let mut screen = Screen::new(6, 2, '.', Style::Window);
        assert_eq!(screen.text(4, 0, "abcd", Style::Window), 2);
        assert_eq!(screen.plain().lines().next().unwrap(), "....ab");
    }

    #[test]
    fn runs_of_one_style_are_joined() {
        let mut screen = Screen::new(6, 1, ' ', Style::Frame);
        screen.text(0, 0, "ab", Style::Window);
        screen.text(2, 0, "cd", Style::Window);
        let rows = screen.rows();
        assert_eq!(rows[0].len(), 2);
        assert_eq!(rows[0][0].text, "abcd");
        assert_eq!(rows[0][0].style, Style::Window);
    }

    #[test]
    fn a_frame_sets_its_title_into_the_top_rule() {
        let mut screen = Screen::new(24, 4, ' ', Style::Window);
        screen.frame(
            Rect::new(0, 0, 24, 4),
            Style::Frame,
            "Projects",
            Style::Title,
        );
        let text = screen.plain();
        let first = text.lines().next().unwrap();
        assert!(first.starts_with('╔'), "{first}");
        assert!(first.ends_with('╗'), "{first}");
        assert!(first.contains("╡ Projects ╞"), "{first}");
        assert!(text.lines().last().unwrap().starts_with('╚'));
    }

    #[test]
    fn a_title_too_long_for_its_frame_is_cut_not_overflowed() {
        let mut screen = Screen::new(20, 3, ' ', Style::Window);
        screen.frame(
            Rect::new(0, 0, 20, 3),
            Style::Frame,
            &"t".repeat(100),
            Style::Title,
        );
        let first = screen.plain().lines().next().unwrap().to_owned();
        assert_eq!(first.chars().count(), 20);
        assert!(first.starts_with('╔') && first.ends_with('╗'));
    }

    #[test]
    fn a_scrollbar_shows_a_full_thumb_when_everything_fits() {
        let mut screen = Screen::new(3, 6, ' ', Style::Window);
        screen.scrollbar(0, 0, 6, 0, 10, 10);
        let text = screen.plain();
        assert!(text.contains('▲') && text.contains('▼'));
        assert_eq!(text.matches('█').count(), 4);
    }

    #[test]
    fn a_scrollbar_thumb_moves_with_the_list() {
        let mut top = Screen::new(1, 8, ' ', Style::Window);
        top.scrollbar(0, 0, 8, 0, 3, 30);
        let mut bottom = Screen::new(1, 8, ' ', Style::Window);
        bottom.scrollbar(0, 0, 8, 27, 3, 30);
        let at = |s: &Screen| s.plain().lines().position(|l| l.contains('█')).unwrap();
        assert!(at(&top) < at(&bottom), "the thumb did not move");
    }

    #[test]
    fn cutting_marks_what_it_removed() {
        assert_eq!(cut("abcdef", 3), "ab…");
        assert_eq!(cut("abc", 3), "abc");
        assert_eq!(cut("abc", 0), "");
    }
}
