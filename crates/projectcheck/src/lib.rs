//! Static checks on a project directory. Nothing here starts a VM, calls
//! Docker or runs project code, and every repo file is treated as hostile.
//!
//! - [`detect`] finds what kind of project a directory holds.
//! - [`compose`] reads a Compose file as data and reports what it asks for.
//! - [`dockerfile`] reads a Dockerfile's ports and writes the Compose file a
//!   project with only a Dockerfile runs as.
//! - [`resources`] reads the repo's resource requests and fits them to the
//!   user's limits and the host.
//!
//! No function here reads a `.env` file or the host environment.

pub mod compose;
pub mod detect;
pub mod dockerfile;
pub mod resources;

mod safe_fs;
mod yaml;

use std::path::PathBuf;

/// Why a repo file could not be read or parsed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is larger than the limit of {limit} bytes")]
    TooLarge { path: PathBuf, limit: u64 },
    #[error("file is not valid UTF-8: {0}")]
    NotUtf8(PathBuf),
    #[error("input is larger than the limit of {limit} bytes")]
    InputTooLarge { limit: u64 },
    #[error("invalid YAML: {0}")]
    Yaml(String),
    #[error("YAML expands to more than {limit} nodes, which looks like an alias bomb")]
    TooComplex { limit: u64 },
    #[error("the file has too many `[` and `{{` for its size to be parsed safely")]
    TooManyFlowCollections,
    #[error("the top level of the file must be a mapping")]
    NotAMapping,
    #[error("path is outside the project: {0}")]
    PathEscapes(PathBuf),
    #[error("refusing to read an environment file: {0}")]
    EnvFileRefused(PathBuf),
    #[error("not a regular file: {0}")]
    NotARegularFile(PathBuf),
    #[error("file changed while it was being opened: {0}")]
    FileChanged(PathBuf),
}

const MAX_TEXT_CHARS: usize = 512;

/// Makes text from a repo safe to show: control, bidi and other invisible
/// characters are escaped, and long text is cut.
pub(crate) fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_TEXT_CHARS));
    for (count, c) in text.chars().enumerate() {
        if count >= MAX_TEXT_CHARS {
            out.push_str("...");
            break;
        }
        match c {
            compose::UNKNOWN_VALUE => out.push_str("<variable>"),
            c if is_unsafe_char(c) => out.extend(c.escape_unicode()),
            c => out.push(c),
        }
    }
    out
}

fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{00AD}' | '\u{061C}' | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF0}'..='\u{FFFB}')
}

#[cfg(test)]
mod tests {
    use super::clean;

    #[test]
    fn clean_escapes_terminal_and_bidi_characters() {
        let out = clean("a\u{1b}[31m\nb\u{202e}c");
        assert!(!out.contains('\u{1b}'));
        assert!(!out.contains('\n'));
        assert!(!out.contains('\u{202e}'));
        assert!(out.contains("\\u{1b}"));
    }

    #[test]
    fn clean_cuts_long_text() {
        assert!(clean(&"x".repeat(100_000)).len() < 600);
    }
}
