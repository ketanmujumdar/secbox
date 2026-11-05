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
    fn ordinary_text_is_left_alone() {
        assert_eq!(escape("web/package.json"), "web/package.json");
    }
}
