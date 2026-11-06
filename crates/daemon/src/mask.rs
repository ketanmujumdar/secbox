//! Hiding secret values in what the daemon sends back from a sandbox.
//!
//! A service that is given a secret can print it, by mistake or on purpose,
//! and a hostile one can put it in a name it looks up. Logs and the list of
//! blocked names both come back through the daemon, so the daemon replaces
//! each assigned value with the secret's name in brackets, such as
//! `[DEMO_TOKEN]`, before any client sees the text.
//!
//! Besides the value as it is, this catches the shapes a thief most often
//! uses to slip one past a plain search: base64 (at every alignment, so the
//! value is found inside a longer encoded string such as `NAME=value`), URL
//! encoding and hex. A value that has been encrypted, split up or otherwise
//! reworked cannot be recognised. This is for leaks into logs, not a defence
//! against a thief, which is the network rules' job.
//!
//! The values live here only while one request is answered, and are wiped
//! when it ends.

use proto::ApiError;
use secrets::SecretName;
use zeroize::Zeroizing;

use crate::{internal, Project, Service};

/// Shorter than this, a value would match ordinary text, and an encoded
/// fragment would match by chance. A value this short is still masked as it
/// is; only its encoded forms are skipped.
const MIN_ENCODED: usize = 8;

/// How much of an unfinished line is held back while following, waiting for
/// the rest of a value that may have been split between two reads.
const MAX_HELD: usize = 64 * 1024;

/// The fixed part of a hostname piece that is compared with a value.
const NAME_WINDOW: usize = 16;

struct Pattern {
    text: Zeroizing<String>,
    label: String,
}

/// Every form of every value to hide, longest first, so that a value inside
/// another leaves no fragment behind.
#[derive(Default)]
pub struct Masker {
    patterns: Vec<Pattern>,
    /// Lower-case letters and digits of each value, for hostnames.
    plain: Vec<(Zeroizing<String>, String)>,
}

// Hand-written so that a stray debug print cannot show the values.
impl std::fmt::Debug for Masker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Masker({} patterns)", self.patterns.len())
    }
}

impl Masker {
    /// The masker for one project's assigned secrets. A secret with no value
    /// stored is skipped. A store that cannot be read is an error, because
    /// carrying on would show the values unmasked.
    pub fn for_project(service: &Service, project: &Project) -> Result<Self, ApiError> {
        let Some(meta) = hoststate::read_meta(&project.dir).map_err(internal)? else {
            return Ok(Self::default());
        };
        let store = service.deps().store(&project.id);
        let mut values = Vec::new();
        for assignment in meta.assignments.iter() {
            let name: &SecretName = assignment.name();
            match store.get(name) {
                Ok(Some(value)) => {
                    values.push((name.as_str().to_owned(), Zeroizing::new(value.expose().to_owned())));
                }
                Ok(None) => {}
                Err(_) => {
                    return Err(internal(
                        "the keychain could not be read, so secret values cannot be hidden. Nothing was shown.",
                    ))
                }
            }
        }
        Ok(Self::new(values))
    }

    pub fn new(values: Vec<(String, Zeroizing<String>)>) -> Self {
        let mut patterns = Vec::new();
        let mut plain = Vec::new();
        for (name, value) in &values {
            if value.is_empty() {
                continue;
            }
            let label = format!("[{name}]");
            let mut forms = vec![Zeroizing::new(value.to_string())];
            if value.len() >= MIN_ENCODED {
                forms.extend(base64_forms(value.as_bytes()));
                forms.push(Zeroizing::new(url_encode(value)));
                forms.push(Zeroizing::new(hex(value.as_bytes(), false)));
                forms.push(Zeroizing::new(hex(value.as_bytes(), true)));
            }
            for text in forms {
                if text.is_empty() || patterns.iter().any(|p: &Pattern| *p.text == *text) {
                    continue;
                }
                patterns.push(Pattern {
                    text,
                    label: label.clone(),
                });
            }
            let letters: String = value
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .map(|c| c.to_ascii_lowercase())
                .collect();
            if letters.len() >= NAME_WINDOW {
                plain.push((Zeroizing::new(letters), label));
            }
        }
        patterns.sort_by_key(|p| std::cmp::Reverse(p.text.len()));
        Self { patterns, plain }
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Replaces every form of every value in `text`.
    pub fn mask(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for pattern in &self.patterns {
            if out.contains(pattern.text.as_str()) {
                out = out.replace(pattern.text.as_str(), &pattern.label);
            }
        }
        out
    }

    /// A hostname the guest looked up. DNS ignores case and a thief can cut a
    /// value into pieces that fit a label, so any label holding sixteen
    /// letters or digits in a row from a value is replaced whole.
    pub fn mask_hostname(&self, name: &str) -> String {
        let masked = self.mask(name);
        if self.plain.is_empty() {
            return masked;
        }
        masked
            .split('.')
            .map(|label| {
                let letters: String = label
                    .chars()
                    .filter(char::is_ascii_alphanumeric)
                    .map(|c| c.to_ascii_lowercase())
                    .collect();
                for (value, tag) in &self.plain {
                    if letters.len() >= NAME_WINDOW
                        && (0..=value.len() - NAME_WINDOW)
                            .any(|at| letters.contains(&value[at..at + NAME_WINDOW]))
                    {
                        return tag.clone();
                    }
                }
                label.to_owned()
            })
            .collect::<Vec<_>>()
            .join(".")
    }
}

/// Masks a stream that arrives in pieces. Whole lines go out at once; the
/// unfinished end of the last line is held until the rest arrives, so a value
/// split between two reads is still found.
#[derive(Default)]
pub struct StreamMasker {
    held: String,
}

impl StreamMasker {
    pub fn push(&mut self, masker: &Masker, text: &str) -> String {
        if masker.is_empty() {
            return text.to_owned();
        }
        self.held.push_str(text);
        let cut = match self.held.rfind('\n') {
            Some(at) => at + 1,
            // A very long line with no end yet goes out anyway, keeping back
            // only as much as the longest value could need.
            None if self.held.len() > MAX_HELD => {
                let keep = masker.patterns.first().map_or(0, |p| p.text.len());
                floor_char(&self.held, self.held.len() - keep)
            }
            None => return String::new(),
        };
        let rest = self.held.split_off(cut);
        let ready = std::mem::replace(&mut self.held, rest);
        masker.mask(&ready)
    }

    pub fn finish(&mut self, masker: &Masker) -> String {
        masker.mask(&std::mem::take(&mut self.held))
    }
}

fn floor_char(text: &str, mut at: usize) -> usize {
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..=chunk.len() {
            out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char);
        }
    }
    out
}

/// The part of the base64 text that the value alone decides, for each of the
/// three ways it can line up inside a longer encoded string.
fn base64_forms(value: &[u8]) -> Vec<Zeroizing<String>> {
    let mut forms = Vec::new();
    for shift in 0..3 {
        let mut padded = Zeroizing::new(vec![0u8; shift]);
        padded.extend_from_slice(value);
        let encoded = Zeroizing::new(base64(&padded));
        // A character covers six bits. Those touching the padding before, or
        // whatever follows after, are not the value's own.
        let first = (shift * 8).div_ceil(6);
        let last = (shift + value.len()) * 8 / 6;
        if last > first && last - first >= MIN_ENCODED {
            forms.push(Zeroizing::new(encoded[first..last].to_owned()));
        }
    }
    forms
}

fn url_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn hex(bytes: &[u8], upper: bool) -> String {
    bytes
        .iter()
        .map(|b| if upper { format!("{b:02X}") } else { format!("{b:02x}") })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "demo_FAKE_ghp_TOKEN_not_real_123456";

    fn masker() -> Masker {
        Masker::new(vec![("DEMO_TOKEN".into(), Zeroizing::new(TOKEN.into()))])
    }

    #[test]
    fn the_value_is_replaced_by_its_name() {
        let out = masker().mask(&format!("printing the token: {TOKEN}\n"));
        assert_eq!(out, "printing the token: [DEMO_TOKEN]\n");
    }

    #[test]
    fn base64_is_caught_inside_a_longer_string_at_every_alignment() {
        let m = masker();
        for prefix in ["", "D", "DE", "DEMO_TOKEN="] {
            let encoded = base64(format!("{prefix}{TOKEN}\n").as_bytes());
            let out = m.mask(&encoded);
            assert!(out.contains("[DEMO_TOKEN]"), "{prefix:?}: {out}");
            assert!(!out.contains(&base64(TOKEN.as_bytes())[4..20]), "{out}");
        }
    }

    #[test]
    fn url_encoding_and_hex_are_caught() {
        let m = Masker::new(vec![("K".into(), Zeroizing::new("p@ss word/123!".into()))]);
        assert_eq!(m.mask("?t=p%40ss%20word%2F123%21"), "?t=[K]");
        assert_eq!(m.mask(&hex(b"p@ss word/123!", false)), "[K]");
        assert_eq!(m.mask(&hex(b"p@ss word/123!", true)), "[K]");
    }

    #[test]
    fn a_short_value_is_masked_as_it_is_but_not_hunted_in_encodings() {
        let m = Masker::new(vec![("PIN".into(), Zeroizing::new("4821".into()))]);
        assert_eq!(m.mask("pin 4821"), "pin [PIN]");
        assert_eq!(m.patterns.len(), 1);
    }

    #[test]
    fn a_value_split_between_two_reads_is_still_masked() {
        let m = masker();
        let mut stream = StreamMasker::default();
        let (a, b) = TOKEN.split_at(10);
        let first = stream.push(&m, &format!("token: {a}"));
        let second = stream.push(&m, &format!("{b} done\nnext"));
        let last = stream.finish(&m);
        assert_eq!(first, "");
        assert_eq!(second, "token: [DEMO_TOKEN] done\n");
        assert_eq!(last, "next");
    }

    #[test]
    fn a_token_cut_into_a_hostname_label_is_hidden() {
        let m = masker();
        // What the demo's DNS probe sends: letters and digits only, cut short.
        let piece: String = TOKEN.chars().filter(char::is_ascii_alphanumeric).take(30).collect();
        let name = format!("{piece}.dns-exfil.example.net");
        assert_eq!(m.mask_hostname(&name), "[DEMO_TOKEN].dns-exfil.example.net");
        assert_eq!(m.mask_hostname(&name.to_uppercase()), "[DEMO_TOKEN].DNS-EXFIL.EXAMPLE.NET");
        assert_eq!(m.mask_hostname("registry.npmjs.org"), "registry.npmjs.org");
    }

    #[test]
    fn with_nothing_to_hide_text_passes_through() {
        let m = Masker::default();
        let mut stream = StreamMasker::default();
        assert_eq!(stream.push(&m, "partial"), "partial");
    }

    #[test]
    fn a_debug_print_shows_no_value() {
        assert!(!format!("{:?}", masker()).contains("demo"));
    }
}
