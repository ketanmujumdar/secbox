//! The terminal itself: raw mode, the alternate screen, and the size.
//!
//! Raw mode is entered through a guard, so the terminal is put back even if
//! the interface stops early. A terminal left in raw mode is a broken shell,
//! which is worse than any error message.

use std::io::{self, IsTerminal, Write};
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

/// The window size.
///
/// The portable way to ask without a system call this crate is allowed to
/// make is to ask the terminal: put the cursor far past the bottom right,
/// where it stops at the real corner, then ask where it is. A terminal that
/// does not answer gets the fallback.
pub fn size() -> (u16, u16) {
    const FALLBACK: (u16, u16) = (80, 24);
    if let (Ok(columns), Ok(rows)) = (var("COLUMNS"), var("LINES")) {
        if columns >= 40 && rows >= 10 {
            return (columns, rows);
        }
    }
    match ask_the_terminal() {
        Some((columns, rows)) if columns >= 40 && rows >= 10 => (columns, rows),
        _ => FALLBACK,
    }
}

fn var(name: &str) -> Result<u16, ()> {
    std::env::var(name)
        .map_err(|_| ())?
        .parse::<u16>()
        .map_err(|_| ())
}

fn ask_the_terminal() -> Option<(u16, u16)> {
    use std::io::Read;
    // Without a terminal on both sides there is nobody to answer, and the
    // read below would block for ever waiting for one.
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return None;
    }
    let mut out = io::stdout();
    out.write_all(b"\x1b[s\x1b[999;999H\x1b[6n").ok()?;
    out.flush().ok()?;
    let mut reply = Vec::new();
    let mut byte = [0u8; 1];
    let mut stdin = io::stdin();
    // The reply ends in `R`. A terminal that never sends one would otherwise
    // hold this open, so the read is capped.
    for _ in 0..32 {
        if stdin.read_exact(&mut byte).is_err() {
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
    /// The query path needs a terminal, so a test can only reach the
    /// environment branch and the fallback. That is also the property that
    /// matters: with nobody to answer, this must return rather than wait.
    #[test]
    fn a_size_the_interface_cannot_use_falls_back() {
        // Driven through the environment, which is the branch a test can
        // reach without a terminal.
        std::env::set_var("COLUMNS", "10");
        std::env::set_var("LINES", "4");
        assert_eq!(super::size(), (80, 24));
        std::env::set_var("COLUMNS", "120");
        std::env::set_var("LINES", "40");
        assert_eq!(super::size(), (120, 40));
        std::env::remove_var("COLUMNS");
        std::env::remove_var("LINES");
    }
}
