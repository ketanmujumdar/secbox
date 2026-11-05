//! Output helpers. Everything from a repo, a path or a tool goes through
//! [`esc`] before it is printed, and color is added only after that.

/// Escapes control, bidi and other invisible characters.
pub fn esc(text: &str) -> String {
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

pub fn esc_path(path: &std::path::Path) -> String {
    esc(&path.to_string_lossy())
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Good,
    Warn,
    Bad,
    Strong,
    Dim,
}

/// Wraps already escaped text in color, if color is on.
pub fn paint(color: bool, tone: Tone, text: &str) -> String {
    if !color {
        return text.to_string();
    }
    let code = match tone {
        Tone::Good => "32",
        Tone::Warn => "33",
        Tone::Bad => "1;31",
        Tone::Strong => "1",
        Tone::Dim => "2",
    };
    format!("\u{1b}[{code}m{text}\u{1b}[0m")
}

/// Pads to a width, then paints, so that color codes do not upset alignment.
pub fn cell(color: bool, tone: Tone, text: &str, width: usize) -> String {
    let pad = width.saturating_sub(text.chars().count());
    format!("{}{}", paint(color, tone, text), " ".repeat(pad))
}

/// `6442450944` becomes `6.0 GB`. Binary units, as Docker counts them.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("TB", 1 << 40),
        ("GB", 1 << 30),
        ("MB", 1 << 20),
        ("KB", 1 << 10),
    ];
    for (unit, size) in UNITS {
        if bytes >= size {
            return format!("{:.1} {unit}", bytes as f64 / size as f64);
        }
    }
    format!("{bytes} B")
}

/// The sentence every refusal and report ends with until VM flows exist.
pub fn not_protecting() -> String {
    format!(
        "{} is an early build. Do not rely on it for untrusted code: containment is not fully tested, and code cannot be exported yet.",
        brand::PRODUCT_NAME
    )
}
