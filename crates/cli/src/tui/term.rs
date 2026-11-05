//! The terminal itself: raw mode, the alternate screen, and the size.
//!
//! Raw mode is entered through a guard, so the terminal is put back even if
//! the interface stops early. A terminal left in raw mode is a broken shell,
//! which is worse than any error message.

use std::io::{self, BufRead, IsTerminal, Write};
use std::os::fd::AsFd;

use nix::sys::termios::{self, LocalFlags, SetArg, Termios};

/// Restores the terminal when dropped.
pub struct RawMode {
    original: Termios,
}

impl RawMode {
    /// Enters raw mode, or returns why it could not.
    pub fn enter() -> Result<Self, String> {
        let stdin = io::stdin();
        if !stdin.is_terminal() || !io::stdout().is_terminal() {
            return Err("this needs a terminal on both input and output".to_owned());
        }
        let fd = stdin.as_fd();
        let original = termios::tcgetattr(fd).map_err(|e| format!("cannot read the terminal: {e}"))?;
        let mut raw = original.clone();
        // Characters arrive as they are typed, nothing is echoed, and the
        // usual interrupt keys are left alone so that Ctrl-C still works.
        raw.local_flags
            .remove(LocalFlags::ECHO | LocalFlags::ICANON);
        termios::tcsetattr(fd, SetArg::TCSANOW, &raw)
            .map_err(|e| format!("cannot set the terminal: {e}"))?;
        let mut out = io::stdout();
        // Alternate screen, cursor hidden.
        let _ = out.write_all(b"\x1b[?1049h\x1b[?25l");
        let _ = out.flush();
        Ok(Self { original })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let mut out = io::stdout();
        // Leave the alternate screen, show the cursor, reset colour.
        let _ = out.write_all(b"\x1b[0m\x1b[?25h\x1b[?1049l");
        let _ = out.flush();
        let _ = termios::tcsetattr(io::stdin().as_fd(), SetArg::TCSANOW, &self.original);
    }
}

/// Puts the terminal settings back when dropped, for a child process that is
/// allowed to change them and might not live to undo that itself.
pub struct SavedSettings {
    original: Termios,
}

impl SavedSettings {
    /// Reads the current settings, if standard input is a terminal.
    pub fn read() -> Option<Self> {
        let stdin = io::stdin();
        if !stdin.is_terminal() {
            return None;
        }
        let original = termios::tcgetattr(stdin.as_fd()).ok()?;
        Some(Self { original })
    }
}

impl Drop for SavedSettings {
    fn drop(&mut self) {
        let _ = termios::tcsetattr(io::stdin().as_fd(), SetArg::TCSANOW, &self.original);
    }
}

/// The window size.
///
/// Both handles are passed in. Nothing here may reach for the process's own
/// standard input, because the caller already holds a lock on it: taking that
/// lock a second time deadlocks, and a deadlocked full-screen interface
/// cannot even be quit.
///
/// The portable way to ask is to ask the terminal: put the cursor far past
/// the bottom right, where it stops at the real corner, then ask where it is.
/// A terminal that does not answer gets the fallback.
pub fn size(
    env: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> (u16, u16) {
    const FALLBACK: (u16, u16) = (80, 24);
    if let (Some(columns), Some(rows)) = (number(env, "COLUMNS"), number(env, "LINES")) {
        if columns >= 40 && rows >= 10 {
            return (columns, rows);
        }
    }
    match ask(input, out) {
        Some((columns, rows)) if columns >= 40 && rows >= 10 => (columns, rows),
        _ => FALLBACK,
    }
}

/// The environment is read through the caller's own lookup, never the
/// process's. A test that set a real variable here would race every other
/// test in the binary, because the environment is shared by all of them.
fn number(env: &dyn Fn(&str) -> Option<std::ffi::OsString>, name: &str) -> Option<u16> {
    env(name)?.to_str()?.parse().ok()
}

fn ask(input: &mut dyn BufRead, out: &mut dyn Write) -> Option<(u16, u16)> {
    // Without a terminal there is nobody to answer, and the read below would
    // wait for ever.
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return None;
    }
    out.write_all(b"\x1b[s\x1b[999;999H\x1b[6n").ok()?;
    out.flush().ok()?;
    let mut reply = Vec::new();
    let mut byte = [0u8; 1];
    // The reply ends in `R`, and is capped so that a terminal which sends
    // something else cannot hold this open.
    for _ in 0..32 {
        if input.read_exact(&mut byte).is_err() {
            break;
        }
        if byte[0] == b'R' {
            break;
        }
        reply.push(byte[0]);
    }
    let _ = out.write_all(b"\x1b[u");
    let text = String::from_utf8(reply).ok()?;
    let digits = text.trim_start_matches(['\x1b', '[']);
    let (rows, columns) = digits.split_once(';')?;
    Some((columns.parse().ok()?, rows.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::io::Cursor;

    fn size_with(columns: Option<&str>, rows: Option<&str>) -> (u16, u16) {
        let columns = columns.map(OsString::from);
        let rows = rows.map(OsString::from);
        let env = |name: &str| match name {
            "COLUMNS" => columns.clone(),
            "LINES" => rows.clone(),
            _ => None,
        };
        let mut input = Cursor::new(Vec::new());
        let mut out: Vec<u8> = Vec::new();
        super::size(&env, &mut input, &mut out)
    }

    #[test]
    fn the_environment_is_used_when_it_gives_a_usable_size() {
        assert_eq!(size_with(Some("120"), Some("40")), (120, 40));
    }

    /// Anything unusable, and anything the terminal will not answer, has to
    /// end in the fallback rather than a wait.
    #[test]
    fn an_unusable_size_falls_back_without_waiting() {
        assert_eq!(size_with(Some("10"), Some("4")), (80, 24));
        assert_eq!(size_with(None, None), (80, 24));
        assert_eq!(size_with(Some("nonsense"), Some("40")), (80, 24));
        assert_eq!(size_with(Some("120"), None), (80, 24));
    }
}
