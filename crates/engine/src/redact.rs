/// Removes sensitive text from command output before it reaches an error.
pub trait Redactor: Send + Sync {
    /// Returns `text` with anything sensitive replaced.
    fn redact(&self, text: &str) -> String;
}

/// Leaves text unchanged.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoRedaction;

impl Redactor for NoRedaction {
    fn redact(&self, text: &str) -> String {
        text.to_owned()
    }
}

/// Replaces each known value with `[REDACTED]`.
#[derive(Default, Clone)]
pub struct LiteralRedactor {
    values: Vec<String>,
}

impl LiteralRedactor {
    /// Empty values are ignored, since they would match everywhere. Longer values
    /// are replaced first so that one value inside another leaves no fragment.
    pub fn new<I, S>(values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut values: Vec<String> = values
            .into_iter()
            .map(Into::into)
            .filter(|v| !v.is_empty())
            .collect();
        values.sort_by_key(|v| std::cmp::Reverse(v.len()));
        Self { values }
    }
}

// Hand-written so that a stray debug print cannot show the values.
impl std::fmt::Debug for LiteralRedactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LiteralRedactor({} values)", self.values.len())
    }
}

impl Redactor for LiteralRedactor {
    fn redact(&self, text: &str) -> String {
        self.values.iter().fold(text.to_owned(), |acc, v| {
            acc.replace(v.as_str(), "[REDACTED]")
        })
    }
}

/// Applies one redactor and then another.
pub(crate) struct Chain<'a>(pub &'a dyn Redactor, pub &'a dyn Redactor);

impl Redactor for Chain<'_> {
    fn redact(&self, text: &str) -> String {
        self.1.redact(&self.0.redact(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_every_occurrence() {
        let r = LiteralRedactor::new(["tok"]);
        assert_eq!(r.redact("a tok b tok"), "a [REDACTED] b [REDACTED]");
    }

    #[test]
    fn longer_value_goes_first() {
        let r = LiteralRedactor::new(["abc", "abcdef"]);
        assert_eq!(r.redact("x abcdef y"), "x [REDACTED] y");
    }

    #[test]
    fn empty_value_is_ignored() {
        let r = LiteralRedactor::new([""]);
        assert_eq!(r.redact("keep"), "keep");
    }

    #[test]
    fn debug_hides_values() {
        let r = LiteralRedactor::new(["hunter2"]);
        assert!(!format!("{r:?}").contains("hunter2"));
    }

    #[test]
    fn chain_applies_both() {
        let a = LiteralRedactor::new(["one"]);
        let b = LiteralRedactor::new(["two"]);
        assert_eq!(
            Chain(&a, &b).redact("one two three"),
            "[REDACTED] [REDACTED] three"
        );
    }

    #[test]
    fn no_redaction_is_identity() {
        assert_eq!(NoRedaction.redact("same"), "same");
    }
}
