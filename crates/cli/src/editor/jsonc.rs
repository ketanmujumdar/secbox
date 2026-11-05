//! Reading the JSON-with-comments files editors and agents write.
//!
//! These files are data, never code. They are read with a size limit, parsed
//! with a plain data parser, and nothing in them is executed or expanded.
//! [`set_top_level`] changes one member by splicing bytes, so comments,
//! trailing commas and formatting elsewhere in the file survive untouched.

use std::fs;
use std::io::Read;
use std::path::Path;

use serde_json::Value;

/// No settings or tasks file an editor writes comes near this. A bigger one is
/// refused rather than read, so a repo cannot hand us a huge file to chew on.
pub const MAX_BYTES: u64 = 1 << 20;
/// How deep a hostile file is followed when looking for a nested key.
pub const MAX_DEPTH: u32 = 32;

/// What was found at a path. Nothing here is a value from the file itself.
pub enum Source {
    Missing,
    /// A link, device or folder. Never opened, so a link cannot aim us
    /// somewhere else and a pipe cannot hold the command open.
    NotRegular,
    Unreadable(String),
    TooLarge,
    Text(String),
}

/// Reads a file without following a link at the final element.
pub fn read(path: &Path) -> Source {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Source::Missing,
        Err(e) => return Source::Unreadable(e.kind().to_string()),
        Ok(meta) if !meta.is_file() => return Source::NotRegular,
        Ok(meta) if meta.len() > MAX_BYTES => return Source::TooLarge,
        Ok(_) => {}
    }
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) => return Source::Unreadable(e.kind().to_string()),
    };
    let mut text = String::new();
    // One byte over the limit, so a file that grew between the two reads is
    // caught here rather than being read half-way and parsed as if complete.
    match file.take(MAX_BYTES + 1).read_to_string(&mut text) {
        Ok(_) if text.len() as u64 > MAX_BYTES => Source::TooLarge,
        Ok(_) => Source::Text(text),
        Err(e) => Source::Unreadable(e.kind().to_string()),
    }
}

/// Parses comments and trailing commas, which editors accept, then the rest
/// as ordinary JSON. The error never quotes the file's content.
pub fn parse(text: &str) -> Result<Value, String> {
    let plain = strip(text)?;
    serde_json::from_str(&plain).map_err(|e| format!("line {}, column {}", e.line(), e.column()))
}

/// Replaces comments and trailing commas with spaces, keeping every other byte
/// and every newline where it was, so parse errors still point at real lines.
fn strip(text: &str) -> Result<String, String> {
    let src = text.as_bytes();
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        match src[i] {
            b'"' => {
                let end = string_end(src, i).ok_or("an unterminated string")?;
                out.extend_from_slice(&src[i..end]);
                i = end;
            }
            b'/' if src.get(i + 1) == Some(&b'/') => {
                while i < src.len() && src[i] != b'\n' {
                    out.push(b' ');
                    i += 1;
                }
            }
            b'/' if src.get(i + 1) == Some(&b'*') => {
                let mut end = i + 2;
                while end < src.len() && !(src[end] == b'*' && src.get(end + 1) == Some(&b'/')) {
                    end += 1;
                }
                if end >= src.len() {
                    return Err("an unterminated comment".to_string());
                }
                for byte in &src[i..end + 2] {
                    out.push(if *byte == b'\n' { b'\n' } else { b' ' });
                }
                i = end + 2;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    drop_trailing_commas(&mut out);
    String::from_utf8(out).map_err(|_| "it is not valid text".to_string())
}

/// A comma with only whitespace between it and a closing bracket. Scanning
/// bytes is safe here because no byte of a multi-byte character is ASCII.
fn drop_trailing_commas(out: &mut [u8]) {
    let mut i = 0;
    while i < out.len() {
        if out[i] == b'"' {
            i = string_end(out, i).unwrap_or(out.len());
            continue;
        }
        if out[i] == b',' {
            let mut j = i + 1;
            while j < out.len() && out[j].is_ascii_whitespace() {
                j += 1;
            }
            if matches!(out.get(j), Some(b'}') | Some(b']')) {
                out[i] = b' ';
            }
        }
        i += 1;
    }
}

/// The index just past the closing quote of the string starting at `start`.
fn string_end(src: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    while i < src.len() {
        match src[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// Walks every object in `value`, including nested ones, and returns a label
/// for each one the test accepts. The test is also given the member name the
/// object sits under, so a match can tell a task from the inner object that
/// holds its options. The depth is capped, so a deeply nested file from a
/// repo cannot run the stack out.
pub fn find_objects(value: &Value, test: &dyn Fn(&Map, Option<&str>) -> bool) -> Vec<String> {
    let mut found = Vec::new();
    walk(value, None, test, 0, &mut found);
    found
}

pub type Map = serde_json::Map<String, Value>;

type Test<'a> = &'a dyn Fn(&Map, Option<&str>) -> bool;

fn walk(value: &Value, under: Option<&str>, test: Test, depth: u32, found: &mut Vec<String>) {
    if depth > MAX_DEPTH {
        return;
    }
    match value {
        Value::Object(map) => {
            if test(map, under) {
                found.push(label(map));
            }
            for (name, child) in map {
                walk(child, Some(name), test, depth + 1, found);
            }
        }
        Value::Array(items) => {
            for child in items {
                walk(child, under, test, depth + 1, found);
            }
        }
        _ => {}
    }
}

/// A name for an object from a hostile file, for printing. The caller escapes.
fn label(map: &Map) -> String {
    for name in ["label", "taskName", "command", "type"] {
        if let Some(text) = map.get(name).and_then(Value::as_str) {
            return format!("{name} {}", cut(text));
        }
    }
    "an unnamed entry".to_string()
}

/// Trims text from a file to something that fits on a line.
pub fn cut(text: &str) -> String {
    const LIMIT: usize = 120;
    if text.chars().count() <= LIMIT {
        return text.to_string();
    }
    let kept: String = text.chars().take(LIMIT).collect();
    format!("{kept}...")
}

/// Sets one top-level member, keeping every other byte of the file. Returns
/// the new text. The value must already be valid JSON.
pub fn set_top_level(text: &str, key: &str, value: &str) -> Result<String, String> {
    let quoted = serde_json::to_string(key).map_err(|_| "cannot encode the key".to_string())?;
    let src = text.as_bytes();
    let mut scan = Scan { src, i: 0 };
    scan.trivia();
    if scan.i >= src.len() {
        return Ok(format!("{{\n    {quoted}: {value}\n}}\n"));
    }
    if src[scan.i] != b'{' {
        return Err("it does not hold a JSON object".to_string());
    }
    let open = scan.i;
    scan.i += 1;
    let mut empty = true;
    loop {
        scan.trivia();
        match scan.src.get(scan.i) {
            None => return Err("it ends before the object closes".to_string()),
            Some(b'}') => break,
            Some(b',') => {
                scan.i += 1;
                continue;
            }
            Some(b'"') => {}
            Some(_) => return Err("there is something unexpected in the object".to_string()),
        }
        empty = false;
        let name = scan.string()?;
        scan.trivia();
        if scan.src.get(scan.i) != Some(&b':') {
            return Err("a member has no value".to_string());
        }
        scan.i += 1;
        scan.trivia();
        let start = scan.i;
        scan.value()?;
        if name == key {
            return Ok(format!("{}{value}{}", &text[..start], &text[scan.i..]));
        }
    }
    let comma = if empty { "" } else { "," };
    Ok(format!(
        "{}\n    {quoted}: {value}{comma}{}",
        &text[..=open],
        &text[open + 1..]
    ))
}

struct Scan<'a> {
    src: &'a [u8],
    i: usize,
}

impl Scan<'_> {
    /// Skips whitespace and both kinds of comment.
    fn trivia(&mut self) {
        while self.i < self.src.len() {
            if self.src[self.i].is_ascii_whitespace() {
                self.i += 1;
            } else if self.src[self.i] == b'/' && self.src.get(self.i + 1) == Some(&b'/') {
                while self.i < self.src.len() && self.src[self.i] != b'\n' {
                    self.i += 1;
                }
            } else if self.src[self.i] == b'/' && self.src.get(self.i + 1) == Some(&b'*') {
                let mut end = self.i + 2;
                while end < self.src.len()
                    && !(self.src[end] == b'*' && self.src.get(end + 1) == Some(&b'/'))
                {
                    end += 1;
                }
                self.i = (end + 2).min(self.src.len());
            } else {
                return;
            }
        }
    }

    /// Reads a string and returns its unescaped text.
    fn string(&mut self) -> Result<String, String> {
        let end = string_end(self.src, self.i).ok_or("an unterminated string")?;
        let raw = std::str::from_utf8(&self.src[self.i..end])
            .map_err(|_| "a name is not valid text".to_string())?;
        let name: String =
            serde_json::from_str(raw).map_err(|_| "a name is not a valid string".to_string())?;
        self.i = end;
        Ok(name)
    }

    /// Steps over one value, leaving `i` just past it.
    fn value(&mut self) -> Result<(), String> {
        match self.src.get(self.i) {
            None => Err("it ends in the middle of a value".to_string()),
            Some(b'"') => self.string().map(|_| ()),
            Some(open @ (b'{' | b'[')) => {
                let close = if *open == b'{' { b'}' } else { b']' };
                let mut depth = 0u32;
                while self.i < self.src.len() {
                    self.trivia();
                    match self.src.get(self.i) {
                        None => break,
                        Some(b'"') => {
                            self.string()?;
                            continue;
                        }
                        Some(byte) if *byte == b'{' || *byte == b'[' => depth += 1,
                        Some(byte) if *byte == b'}' || *byte == b']' => {
                            depth -= 1;
                            if depth == 0 {
                                if *byte != close {
                                    return Err("brackets do not match".to_string());
                                }
                                self.i += 1;
                                return Ok(());
                            }
                        }
                        Some(_) => {}
                    }
                    self.i += 1;
                }
                Err("it ends before a value closes".to_string())
            }
            Some(_) => {
                let start = self.i;
                while self.i < self.src.len()
                    && !matches!(self.src[self.i], b',' | b'}' | b']' | b'/')
                    && !self.src[self.i].is_ascii_whitespace()
                {
                    self.i += 1;
                }
                if self.i == start {
                    return Err("a member has an empty value".to_string());
                }
                Ok(())
            }
        }
    }
}
