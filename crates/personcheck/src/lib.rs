//! Asking the person at this computer to confirm one action.
//!
//! The daemon calls this itself, before it starts a sandbox, opens a hostname
//! or lets a push leave. A client never says "the person agreed": anything
//! running as the same user could say the same. Here the operating system
//! draws the prompt, and a background process cannot answer it for the person.
//!
//! - **macOS:** LocalAuthentication with the device-owner policy: Touch ID,
//!   or the login password where there is no Touch ID or it is locked out.
//!   Each prompt is a new context with no reuse window, so one touch confirms
//!   one action.
//! - **Anywhere else:** there is no way to ask yet, and [`method`] says so.
//!   The caller decides what that means; this crate never pretends.
//!
//! This is the one crate in the workspace that may contain unsafe code, and
//! only in its macOS module, because the prompt is a call into Objective-C.

#![deny(unsafe_code)]

use std::time::Duration;

/// How long a prompt waits for an answer before it is withdrawn.
pub const WAIT: Duration = Duration::from_secs(120);

/// The longest reason passed to the prompt. Longer text is cut, so a long
/// list cannot push the part that matters out of the dialog.
pub const MAX_REASON_CHARS: usize = 240;

/// What the person did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Confirmed,
    /// Cancelled, failed, timed out, or could not be asked. The text says
    /// which and is safe to show.
    Declined(String),
}

/// How the person will be asked on this computer, for showing to them, or
/// `None` if this build cannot ask anyone.
pub fn method() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("Touch ID or your login password")
    } else {
        None
    }
}

/// Turns any text into something fit for the prompt: one line, printable,
/// no longer than [`MAX_REASON_CHARS`].
pub fn clean_reason(reason: &str) -> String {
    let mut out = String::new();
    let mut count = 0;
    let mut last_space = false;
    for ch in reason.chars() {
        let ch = if ch.is_control() || is_invisible(ch) { ' ' } else { ch };
        if ch == ' ' {
            if last_space {
                continue;
            }
            last_space = true;
        } else {
            last_space = false;
        }
        if count == MAX_REASON_CHARS {
            out.push('…');
            break;
        }
        out.push(ch);
        count += 1;
    }
    out.trim().to_owned()
}

/// Characters that change how text around them reads without showing
/// themselves: bidirectional controls and zero-width marks.
fn is_invisible(ch: char) -> bool {
    matches!(ch as u32, 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2069 | 0xFEFF)
}

/// Asks the person to confirm. `reason` completes the system's sentence,
/// which on macOS reads "<program> is trying to <reason>." Blocks until they
/// answer, or for at most [`WAIT`].
pub fn confirm(reason: &str) -> Outcome {
    let reason = clean_reason(reason);
    if reason.is_empty() {
        return Outcome::Declined("there was nothing to confirm".to_owned());
    }
    platform::confirm(&reason, WAIT)
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod platform {
    use std::sync::mpsc;
    use std::time::Duration;

    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSError, NSString};
    use objc2_local_authentication::{LAContext, LAPolicy};

    use super::Outcome;

    pub fn confirm(reason: &str, wait: Duration) -> Outcome {
        let policy = LAPolicy::DeviceOwnerAuthentication;
        // SAFETY: a plain constructor with no arguments.
        let context = unsafe { LAContext::new() };
        // SAFETY: a setter on the context made above. Zero means a recent
        // unlock of the Mac never stands in for this prompt.
        unsafe { context.setTouchIDAuthenticationAllowableReuseDuration(0.0) };
        // SAFETY: a query on the context made above.
        if let Err(error) = unsafe { context.canEvaluatePolicy_error(policy) } {
            return Outcome::Declined(format!("this Mac cannot ask you right now: {}", describe(&error)));
        }

        let (tell, answer) = mpsc::sync_channel::<Result<(), String>>(1);
        // The framework calls this once, on a queue of its own. It only sends
        // on a channel, which is safe from any thread.
        let reply = RcBlock::new(move |ok: Bool, error: *mut NSError| {
            let result = if ok.as_bool() {
                Ok(())
            } else {
                // SAFETY: the framework passes a valid NSError, or null.
                let error = unsafe { error.as_ref() };
                Err(error.map_or_else(|| "not confirmed".to_owned(), describe))
            };
            let _ = tell.try_send(result);
        });
        let text = NSString::from_str(reason);
        // SAFETY: the context, the reason and the block all outlive the call;
        // the block captures only a sender, which is `Send`.
        unsafe { context.evaluatePolicy_localizedReason_reply(policy, &text, &reply) };

        match answer.recv_timeout(wait) {
            Ok(Ok(())) => Outcome::Confirmed,
            Ok(Err(why)) => Outcome::Declined(why),
            Err(_) => {
                // SAFETY: withdraws the prompt this context is showing.
                unsafe { context.invalidate() };
                Outcome::Declined(format!("nobody answered within {} seconds", wait.as_secs()))
            }
        }
    }

    fn describe(error: &NSError) -> String {
        let code = error.code();
        // Known codes are said in words only. The number is kept for the rest,
        // where the system's own text may not be enough to look it up.
        let words = match code {
            -1 => "the fingerprint or password was not accepted",
            -2 => "you cancelled",
            -4 => "the system cancelled the prompt",
            -5 => "this Mac has no login password set",
            -8 => "Touch ID is locked: unlock the Mac with its password first",
            -9 | -10 => "the prompt was withdrawn",
            -1004 => "there is no screen to ask on, such as over SSH",
            _ => {
                let system = error.localizedDescription().to_string();
                return super::clean_reason(&format!("{system} (LocalAuthentication {code})"));
            }
        };
        super::clean_reason(words)
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::time::Duration;

    use super::Outcome;

    pub fn confirm(_reason: &str, _wait: Duration) -> Outcome {
        Outcome::Declined("this computer has no way to ask you yet".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reason_is_one_printable_line() {
        assert_eq!(clean_reason("push\n\x1b[31mmain\x1b[0m\t to  x"), "push [31mmain [0m to x");
        assert_eq!(clean_reason("a\u{202E}b\u{200B}c"), "a b c");
    }

    #[test]
    fn a_long_reason_is_cut_and_marked() {
        let long = "x".repeat(MAX_REASON_CHARS + 50);
        let cut = clean_reason(&long);
        assert_eq!(cut.chars().count(), MAX_REASON_CHARS + 1);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn nothing_to_confirm_is_declined() {
        assert!(matches!(confirm(" \n "), Outcome::Declined(_)));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn without_a_way_to_ask_it_declines_and_says_so() {
        assert_eq!(method(), None);
        assert!(matches!(confirm("start x"), Outcome::Declined(_)));
    }
}
