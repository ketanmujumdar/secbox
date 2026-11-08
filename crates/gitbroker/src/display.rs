//! Hostile names: safe display, and the filesystem folding used for matching.

/// Escaped names longer than this are cut, and the cut is stated in the output.
const MAX_DISPLAY_BYTES: usize = 1024;

/// Render hostile bytes as one quoted, printable ASCII line.
///
/// Everything outside printable ASCII is escaped, including all non-ASCII text.
/// That hides nothing from a reviewer: bidi overrides, zero-width characters
/// and homoglyphs show up as escapes instead of looking like something else.
pub fn escape_for_display(raw: &[u8]) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    let mut rest = raw;
    let mut truncated = false;
    while !rest.is_empty() {
        if out.len() >= MAX_DISPLAY_BYTES {
            truncated = true;
            break;
        }
        match std::str::from_utf8(rest) {
            Ok(text) => {
                push_escaped(&mut out, text, &mut rest);
            }
            Err(err) if err.valid_up_to() > 0 => {
                let (good, _) = rest.split_at(err.valid_up_to());
                let text = std::str::from_utf8(good).unwrap_or("");
                push_escaped(&mut out, text, &mut rest);
            }
            Err(_) => {
                out.push_str(&format!("\\x{:02x}", rest[0]));
                rest = &rest[1..];
            }
        }
    }
    out.push('"');
    if truncated {
        out.push_str(&format!(" (+{} more bytes)", rest.len()));
    }
    out
}

fn push_escaped(out: &mut String, text: &str, rest: &mut &[u8]) {
    for ch in text.chars() {
        if out.len() >= MAX_DISPLAY_BYTES {
            return;
        }
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ' '..='~' => out.push(ch),
            c if (c as u32) < 0x80 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
        }
        *rest = &rest[ch.len_utf8()..];
    }
}

/// Code points that HFS+ ignores in names, or that are invisible to a reader.
pub(crate) fn is_ignorable(ch: char) -> bool {
    matches!(ch as u32,
        0x00AD | 0x034F | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x206F | 0xFE00..=0xFE0F | 0xFEFF)
}

/// Fold one path component the way a case-insensitive filesystem might, so that
/// `.CLAUDE`, `.claude` plus a zero-width joiner, and `ma\u{212a}efile` (Kelvin
/// sign) all match their plain lowercase names.
pub(crate) fn fold_component(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw)
        .chars()
        .filter(|c| !is_ignorable(*c))
        .flat_map(char::to_lowercase)
        .map(|c| if c == '\u{17f}' { 's' } else { c })
        .collect()
}

/// True when a name holds bytes that exist to confuse a reader.
pub(crate) fn is_suspicious_name(raw: &[u8]) -> bool {
    match std::str::from_utf8(raw) {
        Err(_) => true,
        Ok(text) => text.chars().any(|c| c.is_control() || is_ignorable(c)),
    }
}

/// Escape a stderr blob from git, keeping it one bounded line. Its lines are
/// joined with a space and blank ones dropped, so it reads as git wrote it
/// rather than as `\r\n` escapes. Anything else unprintable is still escaped.
pub(crate) fn escape_stderr(raw: &[u8]) -> String {
    let lines: Vec<&[u8]> = raw
        .split(|b| *b == b'\n')
        .map(<[u8]>::trim_ascii)
        .filter(|line| !line.is_empty())
        .collect();
    let shown = escape_for_display(&lines.join(&b' '));
    // Drop the quotes around the whole, which only matter for a name.
    match shown.strip_prefix('"').and_then(|rest| rest.rsplit_once('"')) {
        Some((inner, tail)) => format!("{inner}{tail}"),
        None => shown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_newline_quotes_and_controls() {
        assert_eq!(escape_for_display(b"a\"b\nc"), r#""a\"b\nc""#);
        assert_eq!(escape_for_display(b"\x1b[31mred"), r#""\x1b[31mred""#);
        assert_eq!(escape_for_display(b"back\\slash"), r#""back\\slash""#);
        assert_eq!(escape_for_display(b"del\x7f"), r#""del\x7f""#);
    }

    #[test]
    fn escapes_invalid_utf8_and_all_non_ascii() {
        assert_eq!(escape_for_display(b"\xff\xfeok"), r#""\xff\xfeok""#);
        assert_eq!(
            escape_for_display("a\u{202e}b".as_bytes()),
            r#""a\u{202e}b""#
        );
        assert_eq!(escape_for_display("é".as_bytes()), r#""\u{e9}""#);
    }

    #[test]
    fn display_output_is_always_printable_ascii() {
        let all: Vec<u8> = (0u8..=255).collect();
        let shown = escape_for_display(&all);
        assert!(shown.bytes().all(|b| (0x20..0x7f).contains(&b)), "{shown}");
    }

    #[test]
    fn long_names_state_the_cut() {
        let shown = escape_for_display(&[b'a'; 5000]);
        assert!(shown.len() < 1100);
        assert!(shown.ends_with("more bytes)"), "{shown}");
    }

    #[test]
    fn stderr_reads_as_one_plain_line() {
        let raw = b"No ED25519 host key is known for codeberg.org.\r\nHost key verification failed.\r\nfatal: Could not read from remote repository.\n\nPlease make sure you have the correct access rights\n";
        assert_eq!(
            escape_stderr(raw),
            "No ED25519 host key is known for codeberg.org. Host key verification failed. fatal: Could not read from remote repository. Please make sure you have the correct access rights"
        );
        assert_eq!(escape_stderr(b"a\x1b[2Jb\rc"), r"a\x1b[2Jb\rc");
        assert_eq!(escape_stderr(b""), "");
        assert!(escape_stderr(&[b'a'; 5000]).ends_with("more bytes)"));
    }

    #[test]
    fn folding_matches_filesystem_tricks() {
        assert_eq!(fold_component(b".CLAUDE"), ".claude");
        assert_eq!(fold_component(".c\u{200c}laude".as_bytes()), ".claude");
        assert_eq!(fold_component("ma\u{212a}efile".as_bytes()), "makefile");
        assert_eq!(fold_component(".v\u{17f}code".as_bytes()), ".vscode");
    }
}
