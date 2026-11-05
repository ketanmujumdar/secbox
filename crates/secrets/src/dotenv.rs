use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use zeroize::Zeroizing;

use crate::names::{is_secret_name, SecretName};
use crate::value::SecretValue;

/// The largest accepted `.env` file.
pub const MAX_ENV_FILE_BYTES: usize = 1024 * 1024;

/// One parsed assignment.
#[derive(Debug)]
pub struct EnvEntry {
    pub name: SecretName,
    pub value: SecretValue,
    /// The 1-based line where the assignment starts.
    pub line: usize,
}

/// A parse failure. It holds line numbers and fixed text only, never file
/// content: a malformed line can be a value, for example a pasted key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EnvError {
    #[error("file is larger than the limit of {limit} bytes")]
    TooLarge { limit: usize },
    #[error("not a regular file; symlinks and devices are refused")]
    NotRegularFile,
    #[error("could not read the file: {kind}")]
    Io { kind: io::ErrorKind },
    #[error("line {line}: NUL byte")]
    Nul { line: usize },
    #[error("line {line}: not valid UTF-8")]
    InvalidUtf8 { line: usize },
    #[error("line {line}: expected NAME=value")]
    MissingEquals { line: usize },
    #[error("line {line}: invalid variable name")]
    InvalidName { line: usize },
    #[error("line {line}: unterminated quoted value")]
    UnterminatedQuote { line: usize },
    #[error("line {line}: unsupported escape in double-quoted value")]
    BadEscape { line: usize },
    #[error("line {line}: unexpected text after closing quote")]
    TrailingText { line: usize },
    #[error("line {line}: name already assigned on line {first_line}")]
    DuplicateName { line: usize, first_line: usize },
}

/// Parses `.env` text strictly as data. Nothing is executed and nothing is
/// expanded: `$VAR`, `${VAR}`, `$(cmd)` and backticks stay literal.
///
/// - Blank lines and `#` comments are skipped, and a leading BOM is ignored.
/// - An `export ` prefix is accepted. Blanks around `=` are accepted.
/// - Unquoted values run to the end of the line. Surrounding blanks are
///   trimmed, and a `#` preceded by a blank starts a comment.
/// - Single-quoted values are literal, with no escapes. They can span lines.
/// - Double-quoted values can span lines and support `\n \r \t \\ \" \' \$`
///   and a backslash-escaped backtick. Other escapes are errors, rather than a
///   guess at what the value should be.
/// - A repeated name is an error, so one value cannot silently hide another.
pub fn parse_env(input: &str) -> Result<Vec<EnvEntry>, EnvError> {
    parse_env_bytes(input.as_bytes())
}

/// Same as [`parse_env`], for raw bytes. Checks size, NUL and UTF-8 first.
pub fn parse_env_bytes(input: &[u8]) -> Result<Vec<EnvEntry>, EnvError> {
    if input.len() > MAX_ENV_FILE_BYTES {
        return Err(EnvError::TooLarge {
            limit: MAX_ENV_FILE_BYTES,
        });
    }
    let line_of = |offset: usize| 1 + input[..offset].iter().filter(|b| **b == b'\n').count();
    if let Some(offset) = input.iter().position(|b| *b == 0) {
        return Err(EnvError::Nul {
            line: line_of(offset),
        });
    }
    let text = std::str::from_utf8(input).map_err(|e| EnvError::InvalidUtf8 {
        line: line_of(e.valid_up_to()),
    })?;
    Parser::new(text).run()
}

/// Reads and parses a `.env` file. Refuses symlinks and anything else that is
/// not a regular file, since a repo's `.env` could otherwise point at any file
/// the user can read. The buffer is zeroed after parsing.
pub fn parse_env_file(path: &Path) -> Result<Vec<EnvEntry>, EnvError> {
    let io_error = |e: io::Error| EnvError::Io { kind: e.kind() };
    let before = fs::symlink_metadata(path).map_err(io_error)?;
    if !before.file_type().is_file() {
        return Err(EnvError::NotRegularFile);
    }
    if before.len() > MAX_ENV_FILE_BYTES as u64 {
        return Err(EnvError::TooLarge {
            limit: MAX_ENV_FILE_BYTES,
        });
    }
    let file = File::open(path).map_err(io_error)?;
    let opened = file.metadata().map_err(io_error)?;
    if !opened.file_type().is_file() || !same_file(&before, &opened) {
        return Err(EnvError::NotRegularFile);
    }
    // Reserving the full size up front means the buffer does not reallocate
    // and leave an unwiped copy behind for files of the expected size.
    let mut buffer = Zeroizing::new(Vec::with_capacity(before.len() as usize + 1));
    file.take(MAX_ENV_FILE_BYTES as u64 + 1)
        .read_to_end(&mut buffer)
        .map_err(io_error)?;
    parse_env_bytes(&buffer)
}

#[cfg(unix)]
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_file(_: &fs::Metadata, _: &fs::Metadata) -> bool {
    false
}

struct Parser<'a> {
    rest: &'a str,
    line: usize,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            rest: text.strip_prefix('\u{feff}').unwrap_or(text),
            line: 1,
        }
    }

    fn run(mut self) -> Result<Vec<EnvEntry>, EnvError> {
        let mut entries = Vec::new();
        let mut first_seen: HashMap<String, usize> = HashMap::new();
        while !self.rest.is_empty() {
            self.skip_blanks();
            if self.rest.starts_with('#') || self.at_line_end() {
                self.skip_line();
                continue;
            }
            let entry = self.assignment()?;
            let name = entry.name.as_str().to_owned();
            if let Some(first_line) = first_seen.get(&name) {
                return Err(EnvError::DuplicateName {
                    line: entry.line,
                    first_line: *first_line,
                });
            }
            first_seen.insert(name, entry.line);
            entries.push(entry);
        }
        Ok(entries)
    }

    fn assignment(&mut self) -> Result<EnvEntry, EnvError> {
        let line = self.line;
        self.skip_export();
        let name_len = self
            .rest
            .find(|c: char| c == '=' || is_blank(c) || c == '\n' || c == '\r')
            .unwrap_or(self.rest.len());
        let (raw_name, after_name) = self.rest.split_at(name_len);
        self.rest = after_name;
        self.skip_blanks();
        let Some(after_equals) = self.rest.strip_prefix('=') else {
            return Err(EnvError::MissingEquals { line });
        };
        self.rest = after_equals;
        if !is_secret_name(raw_name) {
            return Err(EnvError::InvalidName { line });
        }
        let name = SecretName::new(raw_name).map_err(|_| EnvError::InvalidName { line })?;

        let blank_before_value = self.rest.starts_with(is_blank);
        self.skip_blanks();
        let value = if let Some(body) = self.rest.strip_prefix('\'') {
            self.rest = body;
            let value = self.single_quoted(line)?;
            self.finish_quoted_line()?;
            value
        } else if let Some(body) = self.rest.strip_prefix('"') {
            self.rest = body;
            let value = self.double_quoted(line)?;
            self.finish_quoted_line()?;
            value
        } else {
            self.unquoted(blank_before_value)
        };
        Ok(EnvEntry { name, value, line })
    }

    fn skip_blanks(&mut self) {
        self.rest = self.rest.trim_start_matches(is_blank);
    }

    fn at_line_end(&self) -> bool {
        self.rest.is_empty() || self.rest.starts_with('\n') || self.rest.starts_with("\r\n")
    }

    /// Skips the rest of the current line, including its newline.
    fn skip_line(&mut self) {
        match self.rest.find('\n') {
            Some(index) => {
                self.rest = &self.rest[index + 1..];
                self.line += 1;
            }
            None => self.rest = "",
        }
    }

    fn skip_export(&mut self) {
        if let Some(after) = self.rest.strip_prefix("export") {
            if after.starts_with(is_blank) {
                self.rest = after;
                self.skip_blanks();
            }
        }
    }

    fn unquoted(&mut self, blank_before_value: bool) -> SecretValue {
        let line_len = self.rest.find('\n').unwrap_or(self.rest.len());
        let line = &self.rest[..line_len];
        let mut previous_blank = blank_before_value;
        let mut end = line.len();
        for (index, c) in line.char_indices() {
            if c == '#' && previous_blank {
                end = index;
                break;
            }
            previous_blank = is_blank(c);
        }
        let value = line[..end].trim_end_matches(|c| is_blank(c) || c == '\r');
        let value = SecretValue::new(value);
        self.skip_line();
        value
    }

    fn single_quoted(&mut self, start_line: usize) -> Result<SecretValue, EnvError> {
        let end = self
            .rest
            .find('\'')
            .ok_or(EnvError::UnterminatedQuote { line: start_line })?;
        let body = &self.rest[..end];
        self.line += count_newlines(body);
        self.rest = &self.rest[end + 1..];
        Ok(SecretValue::new(body))
    }

    fn double_quoted(&mut self, start_line: usize) -> Result<SecretValue, EnvError> {
        // Find the closing quote first, so that the output buffer can be sized
        // once and never reallocates, which would leave an unwiped copy.
        let bytes = self.rest.as_bytes();
        let mut index = 0;
        let end = loop {
            match bytes.get(index) {
                None => return Err(EnvError::UnterminatedQuote { line: start_line }),
                Some(b'"') => break index,
                Some(b'\\') => index += 2,
                Some(_) => index += 1,
            }
        };
        let body = &self.rest[..end];
        let mut out = Zeroizing::new(String::with_capacity(body.len()));
        let mut line = self.line;
        let mut chars = body.chars();
        while let Some(c) = chars.next() {
            if c == '\n' {
                line += 1;
            }
            if c != '\\' {
                out.push(c);
                continue;
            }
            out.push(match chars.next() {
                Some('n') => '\n',
                Some('r') => '\r',
                Some('t') => '\t',
                Some(c @ ('\\' | '"' | '\'' | '$' | '`')) => c,
                _ => return Err(EnvError::BadEscape { line }),
            });
        }
        self.line = line;
        self.rest = &self.rest[end + 1..];
        Ok(SecretValue::from_zeroizing(out))
    }

    /// After a closing quote, only blanks and an optional comment may follow.
    fn finish_quoted_line(&mut self) -> Result<(), EnvError> {
        self.skip_blanks();
        if self.rest.starts_with('#') || self.at_line_end() {
            self.skip_line();
            Ok(())
        } else {
            Err(EnvError::TrailingText { line: self.line })
        }
    }
}

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

fn count_newlines(text: &str) -> usize {
    text.bytes().filter(|b| *b == b'\n').count()
}
