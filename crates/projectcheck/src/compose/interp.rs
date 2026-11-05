//! Compose variable syntax, resolved from defaults alone. The host environment
//! and `.env` files are never consulted, so a value without a default stays
//! unknown and is marked with [`UNKNOWN_VALUE`].

/// Stands in for a value that cannot be known statically.
pub const UNKNOWN_VALUE: char = '\u{FFFC}';

const MAX_DEPTH: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Interpolated {
    /// The text with defaults applied and unknown parts marked.
    pub value: String,
    /// A variable appears, so the real value may differ from `value`.
    pub dynamic: bool,
    /// Part of `value` is unknown.
    pub unresolved: bool,
}

pub(super) fn interpolate(text: &str) -> Interpolated {
    let mut out = Interpolated {
        value: String::with_capacity(text.len()),
        dynamic: false,
        unresolved: false,
    };
    expand(text, 0, &mut out);
    out.unresolved = out.value.contains(UNKNOWN_VALUE);
    out
}

fn expand(text: &str, depth: usize, out: &mut Interpolated) {
    let mut rest = text;
    while let Some(index) = rest.find('$') {
        out.value.push_str(&rest[..index]);
        let after = &rest[index + 1..];
        if let Some(tail) = after.strip_prefix('$') {
            out.value.push('$');
            rest = tail;
        } else if let Some(body) = after.strip_prefix('{') {
            out.dynamic = true;
            let Some(end) = closing_brace(body) else {
                out.value.push(UNKNOWN_VALUE);
                return;
            };
            expand_braced(&body[..end], depth, out);
            rest = &body[end + 1..];
        } else {
            let name_len = variable_name_len(after);
            if name_len == 0 {
                out.value.push('$');
                rest = after;
            } else {
                out.dynamic = true;
                out.value.push(UNKNOWN_VALUE);
                rest = &after[name_len..];
            }
        }
    }
    out.value.push_str(rest);
}

fn expand_braced(body: &str, depth: usize, out: &mut Interpolated) {
    let name_len = variable_name_len(body);
    let operator = &body[name_len..];
    let default = operator
        .strip_prefix(":-")
        .or_else(|| operator.strip_prefix('-'));
    match default {
        Some(word) if name_len > 0 && depth < MAX_DEPTH => expand(word, depth + 1, out),
        // No default, a required or `+` form, malformed text or too much
        // nesting: the value cannot be known.
        _ => out.value.push(UNKNOWN_VALUE),
    }
}

/// Finds the `}` that closes a `${`, allowing nested `${...}` in defaults.
fn closing_brace(body: &str) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut open = 0usize;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'$' if bytes.get(index + 1) == Some(&b'{') => {
                open += 1;
                index += 1;
            }
            b'}' if open == 0 => return Some(index),
            b'}' => open -= 1,
            _ => {}
        }
        index += 1;
    }
    None
}

fn variable_name_len(text: &str) -> usize {
    let bytes = text.as_bytes();
    if !bytes
        .first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
    {
        return 0;
    }
    bytes
        .iter()
        .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_')
        .count()
}

/// Splits on `sep` outside `${...}`, so the `:` in `${A:-1}` does not split.
pub(super) fn split_outside_vars(text: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut open = 0usize;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        if c == '$' && chars.peek().is_some_and(|(_, next)| *next == '{') {
            open += 1;
            chars.next();
        } else if c == '}' && open > 0 {
            open -= 1;
        } else if c == sep && open == 0 {
            parts.push(&text[start..index]);
            start = index + c.len_utf8();
        }
    }
    parts.push(&text[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str) -> (String, bool, bool) {
        let out = interpolate(text);
        (out.value, out.dynamic, out.unresolved)
    }

    #[test]
    fn plain_text_is_unchanged() {
        assert_eq!(value("8080:80"), ("8080:80".into(), false, false));
    }

    #[test]
    fn defaults_apply() {
        assert_eq!(value("${A:-5432}:5432"), ("5432:5432".into(), true, false));
        assert_eq!(value("${A-x}"), ("x".into(), true, false));
        assert_eq!(value("${A:-}"), (String::new(), true, false));
    }

    #[test]
    fn nested_defaults_apply() {
        assert_eq!(value("${A:-${B:-7}}"), ("7".into(), true, false));
        assert!(value("${A:-${B}}").2);
    }

    #[test]
    fn forms_without_a_known_value_are_unknown() {
        for text in [
            "${A}",
            "$A",
            "${A:?required}",
            "${A?required}",
            "${A:+alt}",
            "${A+alt}",
            "${",
            "${}",
            "${:-x}",
            "${1A:-x}",
            "x\u{FFFC}",
        ] {
            assert!(value(text).2, "{text}");
        }
    }

    #[test]
    fn double_dollar_is_a_literal() {
        assert_eq!(value("$$A"), ("$A".into(), false, false));
        assert_eq!(value("a$"), ("a$".into(), false, false));
    }

    #[test]
    fn deep_nesting_is_bounded() {
        let text = format!("{}x{}", "${A:-".repeat(5000), "}".repeat(5000));
        assert!(value(&text).2);
    }

    #[test]
    fn split_ignores_separators_in_variables() {
        assert_eq!(
            split_outside_vars("${A:-1}:${B:-${C:-2}}:3", ':'),
            vec!["${A:-1}", "${B:-${C:-2}}", "3"]
        );
    }
}
