//! Making text safe to display.
//!
//! Everything that crosses the socket may have come from a project file, a
//! sandbox or a tool's output, so the daemon escapes it before it ever reaches
//! a client. Clients escape again on the way out, because a second pass on
//! already safe text changes nothing.

/// Escapes control, bidi and other invisible characters.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if is_unsafe(c) {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    out
}

/// Escapes and then cuts to `max_chars`, so one hostile line cannot fill a
/// screen or a log file.
pub fn escape_short(text: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max_chars));
    for (count, c) in text.chars().enumerate() {
        if count >= max_chars {
            out.push_str("...");
            break;
        }
        if is_unsafe(c) {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    out
}

/// Like [`escape_short`], but line breaks stay. Everything else that could
/// move the cursor or recolour a terminal is escaped. A client indents every
/// line after the first, so no line can pass for one of its own.
pub fn escape_keeping_breaks(text: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max_chars));
    for (count, c) in text.chars().enumerate() {
        if count >= max_chars {
            out.push_str("...");
            break;
        }
        if c != '\n' && is_unsafe(c) {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    out
}

/// The last `max_lines` lines of a tool's output, each escaped and cut to
/// `max_chars`, joined by real line breaks. Nothing but those breaks is left
/// unescaped, so the text cannot move the cursor or recolour a terminal. A
/// client indents every line after the first, so none can pass for its own.
pub fn escape_last_lines(text: &str, max_lines: usize, max_chars: usize) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.trim().is_empty())
        .collect();
    let skipped = lines.len().saturating_sub(max_lines);
    let mut out: Vec<String> = Vec::new();
    if skipped > 0 {
        out.push(format!("({skipped} earlier lines left out)"));
    }
    out.extend(lines[skipped..].iter().map(|line| escape_short(line, max_chars)));
    out.join("\n")
}

/// Escapes one line of a file for reading, cut to `max_chars`. Tabs stay, since
/// code is laid out with them and they cannot rewrite what is on screen. A cut
/// says so, because the part past it has not been read.
pub fn escape_line(text: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max_chars));
    for (count, c) in text.chars().enumerate() {
        if count >= max_chars {
            out.push_str(" ... (line cut here)");
            break;
        }
        if c != '\t' && is_unsafe(c) {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    out
}

pub fn escape_path(path: &std::path::Path) -> String {
    escape(&path.to_string_lossy())
}

fn is_unsafe(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{00AD}' | '\u{061C}' | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF0}'..='\u{FFFC}')
}

/// The longest control sequence followed before it is treated as ordinary
/// text. Whatever is left of it then prints harmlessly, without its escape.
const MAX_SEQUENCE_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterState {
    Text,
    /// Just after an escape character.
    Escape,
    /// An escape sequence's intermediate bytes, such as the `(` in `ESC ( B`.
    Intermediate,
    /// A control sequence: parameters, then one final byte.
    Control,
    /// A string sequence: a title, a clipboard write, a device command.
    Str,
}

/// Makes a stream of untrusted bytes safe to print, keeping its text, tabs and
/// newlines.
///
/// Terminal sequences are removed, other control and invisible characters are
/// shown as visible escapes, and a bare carriage return becomes a newline so
/// that a line cannot be written over another. Output arrives in chunks that
/// can end anywhere, so an unfinished sequence or character carries over to
/// the next call. Its output contains nothing it would change, so a second
/// pass leaves it as it is.
///
/// A string sequence ends at a newline as well as at its proper terminator.
/// A terminal would keep reading, but the escape that began it is gone by
/// then, and this way one unterminated title cannot hide every later line.
#[derive(Debug)]
pub struct TerminalFilter {
    state: FilterState,
    /// The start of a character whose remaining bytes have not arrived.
    partial: Vec<u8>,
    /// A carriage return, held to see whether a newline follows it.
    return_held: bool,
    sequence_bytes: usize,
    /// Inside a string sequence, the last byte could begin the two-byte form
    /// of its terminator.
    after_c2: bool,
}

impl Default for TerminalFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalFilter {
    pub fn new() -> Self {
        Self {
            state: FilterState::Text,
            partial: Vec::with_capacity(4),
            return_held: false,
            sequence_bytes: 0,
            after_c2: false,
        }
    }

    /// Filters one chunk and returns what is safe to print so far.
    pub fn push(&mut self, input: &[u8]) -> String {
        let mut out = String::with_capacity(input.len());
        for &byte in input {
            self.byte(byte, &mut out);
        }
        out
    }

    /// Ends the stream. An unfinished character shows as a replacement mark,
    /// and an unfinished sequence is dropped.
    pub fn finish(&mut self) -> String {
        let mut out = String::new();
        self.release_return(&mut out);
        if !self.partial.is_empty() {
            self.partial.clear();
            out.push(char::REPLACEMENT_CHARACTER);
        }
        self.enter(FilterState::Text);
        out
    }

    fn enter(&mut self, state: FilterState) {
        self.state = state;
        self.sequence_bytes = 0;
        self.after_c2 = false;
    }

    fn over_limit(&mut self) -> bool {
        self.sequence_bytes += 1;
        self.sequence_bytes > MAX_SEQUENCE_BYTES
    }

    fn release_return(&mut self, out: &mut String) {
        if self.return_held {
            self.return_held = false;
            out.push('\n');
        }
    }

    fn byte(&mut self, byte: u8, out: &mut String) {
        match self.state {
            FilterState::Text => self.text_byte(byte, out),
            FilterState::Escape => match byte {
                b'[' => self.enter(FilterState::Control),
                b']' | b'P' | b'X' | b'^' | b'_' => self.enter(FilterState::Str),
                0x20..=0x2f => self.enter(FilterState::Intermediate),
                0x30..=0x7e => self.enter(FilterState::Text),
                0x1b => {}
                _ => {
                    self.enter(FilterState::Text);
                    self.text_byte(byte, out);
                }
            },
            FilterState::Intermediate => match byte {
                0x20..=0x2f if !self.over_limit() => {}
                0x30..=0x7e => self.enter(FilterState::Text),
                _ => {
                    self.enter(FilterState::Text);
                    self.text_byte(byte, out);
                }
            },
            FilterState::Control => match byte {
                0x20..=0x3f if !self.over_limit() => {}
                0x40..=0x7e | 0x18 | 0x1a => self.enter(FilterState::Text),
                0x1b => self.enter(FilterState::Escape),
                _ => {
                    self.enter(FilterState::Text);
                    self.text_byte(byte, out);
                }
            },
            FilterState::Str => match byte {
                0x07 | 0x18 | 0x1a => self.enter(FilterState::Text),
                0x1b => self.enter(FilterState::Escape),
                b'\n' => {
                    self.enter(FilterState::Text);
                    self.text_byte(byte, out);
                }
                0x9c if self.after_c2 => self.enter(FilterState::Text),
                _ => {
                    self.after_c2 = byte == 0xc2;
                    if self.over_limit() {
                        self.enter(FilterState::Text);
                    }
                }
            },
        }
    }

    fn text_byte(&mut self, byte: u8, out: &mut String) {
        if self.return_held {
            self.return_held = false;
            out.push('\n');
            if byte == b'\n' {
                return;
            }
        }
        if !self.partial.is_empty() {
            if byte & 0xc0 == 0x80 {
                self.partial.push(byte);
                if self.partial.len() == utf8_width(self.partial[0]) {
                    match std::str::from_utf8(&self.partial) {
                        Ok(text) => {
                            let found = text.chars().next();
                            self.partial.clear();
                            if let Some(c) = found {
                                self.character(c, out);
                            }
                        }
                        Err(_) => {
                            self.partial.clear();
                            out.push(char::REPLACEMENT_CHARACTER);
                        }
                    }
                }
                return;
            }
            self.partial.clear();
            out.push(char::REPLACEMENT_CHARACTER);
        }
        match byte {
            b'\n' | b'\t' => out.push(byte as char),
            b'\r' => self.return_held = true,
            0x1b => self.enter(FilterState::Escape),
            0x00..=0x1f | 0x7f => out.extend((byte as char).escape_unicode()),
            0x20..=0x7e => out.push(byte as char),
            0xc2..=0xf4 => self.partial.push(byte),
            _ => out.push(char::REPLACEMENT_CHARACTER),
        }
    }

    /// One whole character. The eight-bit forms of the sequence openers mean
    /// the same as their escape forms to a terminal that honours them.
    fn character(&mut self, c: char, out: &mut String) {
        match c {
            '\u{9b}' => self.enter(FilterState::Control),
            '\u{90}' | '\u{98}' | '\u{9d}' | '\u{9e}' | '\u{9f}' => self.enter(FilterState::Str),
            c if is_unsafe(c) => out.extend(c.escape_unicode()),
            c => out.push(c),
        }
    }
}

fn utf8_width(lead: u8) -> usize {
    match lead {
        0xf0..=0xff => 4,
        0xe0..=0xef => 3,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_terminal_and_bidi_characters() {
        let out = escape("a\u{1b}[31m\nb\u{202e}c");
        assert!(!out.contains('\u{1b}'));
        assert!(!out.contains('\n'));
        assert!(!out.contains('\u{202e}'));
        assert!(out.contains("\\u{1b}"));
    }

    #[test]
    fn last_lines_keep_their_breaks_and_nothing_else() {
        let out = escape_last_lines("one\r\n\ntwo \u{1b}[2J\nthree\nfour\n", 3, 100);
        assert_eq!(out, "(1 earlier lines left out)\ntwo \\u{1b}[2J\nthree\nfour");
        assert_eq!(escape_last_lines("a\nb", 5, 100), "a\nb");
        assert_eq!(escape_last_lines("abcdef", 5, 3), "abc...");
        assert_eq!(escape_keeping_breaks("a\nb\r\u{1b}", 100), "a\nb\\u{d}\\u{1b}");
    }

    #[test]
    fn escaping_twice_changes_nothing() {
        let once = escape("a\u{1b}[31m\u{202e}b");
        assert_eq!(escape(&once), once);
    }

    #[test]
    fn short_form_cuts_long_text() {
        assert_eq!(escape_short("abcdef", 3), "abc...");
        assert_eq!(escape_short("abc", 3), "abc");
    }

    #[test]
    fn a_line_keeps_its_tabs_and_nothing_else_unsafe() {
        let out = escape_line("\tif (a\u{202e}) {\u{1b}[2J\r", 100);
        assert!(out.starts_with('\t'));
        assert!(out.contains("\\u{202e}") && out.contains("\\u{1b}") && out.contains("\\u{d}"));
        assert!(!out.contains('\u{1b}') && !out.contains('\u{202e}') && !out.contains('\r'));
        assert_eq!(escape_line(&out, 100), out);
    }

    #[test]
    fn a_cut_line_says_it_was_cut() {
        assert_eq!(escape_line("abc", 3), "abc");
        assert_eq!(escape_line("abcd", 3), "abc ... (line cut here)");
    }

    #[test]
    fn ordinary_text_is_left_alone() {
        assert_eq!(escape("web/package.json"), "web/package.json");
    }

    fn filtered(chunks: &[&[u8]]) -> String {
        let mut filter = TerminalFilter::new();
        let mut out = String::new();
        for chunk in chunks {
            out.push_str(&filter.push(chunk));
        }
        out.push_str(&filter.finish());
        out
    }

    fn every_split(input: &[u8]) -> Vec<String> {
        (0..=input.len())
            .map(|at| filtered(&[&input[..at], &input[at..]]))
            .collect()
    }

    #[test]
    fn the_filter_keeps_text_tabs_and_newlines() {
        assert_eq!(filtered(&[b"a\tb\nc d\n"]), "a\tb\nc d\n");
        assert_eq!(filtered(&["caf\u{e9} \u{1f600}\n".as_bytes()]), "caf\u{e9} \u{1f600}\n");
    }

    #[test]
    fn the_filter_removes_every_kind_of_sequence() {
        for (input, want) in [
            (&b"\x1b[31mred\x1b[0m"[..], "red"),
            (b"\x1b[2J\x1b[H\x1b[?1049hx", "x"),
            (b"\x1b]0;title\x07x", "x"),
            (b"\x1b]52;c;aGk=\x1b\\x", "x"),
            (b"\x1bPq#0;2;0;0;0\x1b\\x", "x"),
            (b"\x1b_app\x1b\\x\x1b^pm\x1b\\y\x1bXsos\x1b\\z", "xyz"),
            (b"\x1b(B\x1b#8\x1bc\x1bMx", "x"),
            (b"\xc2\x9b31mx", "x"),
            (b"\xc2\x9d0;title\xc2\x9cx", "x"),
            (b"\xc2\x9bq\xc2\x90dcs\x07x", "x"),
            (b"\x1b\x1b[0mx", "x"),
        ] {
            assert_eq!(filtered(&[input]), want, "{input:?}");
        }
    }

    #[test]
    fn a_sequence_split_at_any_point_is_still_removed() {
        let input = b"a\x1b[38;5;196mb\x1b]0;evil title\x07c\x1b]52;c;QQ==\x1b\\d\xc2\x9b2Je";
        for out in every_split(input) {
            assert_eq!(out, "abcde");
        }
    }

    #[test]
    fn a_character_split_at_any_point_survives() {
        let input = "x\u{e9}\u{20ac}\u{1f600}y\n".as_bytes();
        for out in every_split(input) {
            assert_eq!(out, "x\u{e9}\u{20ac}\u{1f600}y\n");
        }
        let one_at_a_time: Vec<&[u8]> = input.chunks(1).collect();
        assert_eq!(filtered(&one_at_a_time), "x\u{e9}\u{20ac}\u{1f600}y\n");
    }

    #[test]
    fn bare_controls_become_visible_and_cannot_rewrite_a_line() {
        assert_eq!(filtered(&[b"a\x07b\x08c\x7f"]), "a\\u{7}b\\u{8}c\\u{7f}");
        assert_eq!(filtered(&[b"fake\rreal\r\n"]), "fake\nreal\n");
        for out in every_split(b"one\r\ntwo\n") {
            assert_eq!(out, "one\ntwo\n");
        }
        assert_eq!(filtered(&[b"end\r"]), "end\n");
    }

    #[test]
    fn bidi_and_invisible_characters_become_visible() {
        let out = filtered(&["a\u{202e}b\u{200b}c\u{85}d".as_bytes()]);
        assert_eq!(out, "a\\u{202e}b\\u{200b}c\\u{85}d");
    }

    #[test]
    fn invalid_bytes_cannot_get_through() {
        for input in [
            &b"\xff\xfe\x80"[..],
            b"\xc0\xaf",
            b"\xed\xa0\x80",
            b"\xf5\x80\x80\x80",
            b"\xe2\x82",
            b"\xe2\x82\x1b[31m",
        ] {
            let out = filtered(&[input]);
            assert!(out.chars().all(|c| !c.is_control()), "{input:?}: {out:?}");
            assert!(out.contains(char::REPLACEMENT_CHARACTER), "{input:?}: {out:?}");
        }
    }

    #[test]
    fn an_unterminated_string_cannot_hide_later_lines() {
        assert_eq!(filtered(&[b"\x1b]0;never ends\nlater\n"]), "\nlater\n");
        assert_eq!(filtered(&[b"\x1bPnever ends\nlater\n"]), "\nlater\n");
    }

    #[test]
    fn an_endless_sequence_stops_being_swallowed_and_stays_harmless() {
        let mut input = b"\x1b]".to_vec();
        input.extend(std::iter::repeat_n(b'a', MAX_SEQUENCE_BYTES + 10));
        let out = filtered(&[&input]);
        assert!(!out.contains('\u{1b}'));
        assert!(out.len() < 20, "{}", out.len());

        let mut input = b"\x1b[".to_vec();
        input.extend(std::iter::repeat_n(b'1', MAX_SEQUENCE_BYTES + 10));
        input.extend_from_slice(b"mx");
        let out = filtered(&[&input]);
        assert!(!out.contains('\u{1b}'));
        assert!(out.ends_with('x'));
    }

    #[test]
    fn no_hostile_input_leaves_a_control_character_in_the_output() {
        // Every byte value after every sequence opener, whole and split.
        for opener in [&b"\x1b"[..], b"\x1b[", b"\x1b]", b"\x1bP", b"\x1b(", b"\xc2\x9b", b"\xc2"] {
            for value in 0..=255u8 {
                let mut input = opener.to_vec();
                input.push(value);
                input.extend_from_slice(b"tail\x1b[0m\x07");
                for out in every_split(&input) {
                    let bad = out.chars().find(|c| c.is_control() && *c != '\n' && *c != '\t');
                    assert_eq!(bad, None, "{input:?} gave {out:?}");
                }
            }
        }
    }

    #[test]
    fn filtering_twice_changes_nothing() {
        let once = filtered(&[b"a\x1b[31m\x07\r\t\n\xc2\x9b0m\xff\x1b]0;x\x07b"]);
        assert_eq!(filtered(&[once.as_bytes()]), once);
    }
}
