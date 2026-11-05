//! Strict validators. These are allowlists: anything unexpected is rejected.

use crate::display::escape_for_display;
use crate::error::{Error, Result};

const MAX_TASK_LEN: usize = 64;
const MAX_REF_LEN: usize = 200;
const MAX_URL_LEN: usize = 2048;

/// A full object id: 40 or 64 lowercase hex. Short ids and revision syntax are
/// rejected, so nothing that reaches argv can be read as an option or a range.
pub(crate) fn validate_commit_id(id: &str) -> Result<()> {
    let hex = id
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if hex && (id.len() == 40 || id.len() == 64) {
        Ok(())
    } else {
        Err(Error::InvalidCommitId(escape_for_display(id.as_bytes())))
    }
}

/// Validate a ref name from a bundle header. Only `HEAD`, `refs/heads/*` and
/// `refs/tags/*` pass, so `refs/replace/*` and similar can never be imported.
pub(crate) fn validate_bundle_ref(raw: &str) -> Result<()> {
    let bad = || Error::BadRefName(escape_for_display(raw.as_bytes()));
    if raw == "HEAD" {
        return Ok(());
    }
    if raw.len() > MAX_REF_LEN {
        return Err(bad());
    }
    let rest = raw
        .strip_prefix("refs/heads/")
        .or_else(|| raw.strip_prefix("refs/tags/"))
        .ok_or_else(bad)?;
    if rest.is_empty() {
        return Err(bad());
    }
    for part in rest.split('/') {
        let chars_ok = part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if part.is_empty()
            || !chars_ok
            || part.starts_with('-')
            || part.starts_with('.')
            || part.ends_with('.')
            || part.ends_with(".lock")
            || part.contains("..")
        {
            return Err(bad());
        }
    }
    Ok(())
}

/// Validate a task name: lowercase ASCII letters and digits, with `-`, `_` and
/// `.` inside. Lowercase only, because refs are files on a case-insensitive
/// filesystem, and `Fix` and `fix` must not name different branches.
pub fn validate_task_name(task: &str) -> Result<()> {
    let bad = || Error::InvalidTaskName(escape_for_display(task.as_bytes()));
    if task.is_empty() || task.len() > MAX_TASK_LEN {
        return Err(bad());
    }
    let first = task.as_bytes()[0];
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(bad());
    }
    let chars_ok = task
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.'));
    if !chars_ok || task.contains("..") || task.ends_with('.') || task.ends_with(".lock") {
        return Err(bad());
    }
    Ok(())
}

/// Validate a remote URL. Only `https://` passes, plus absolute local paths when
/// `allow_local` is set. The allowlist is what rejects `ext::`, `fd::`, any other
/// `helper::` form, a leading `-`, `ssh://` and scp syntax. SSH is rejected
/// because `core.sshCommand=false` would make it fail anyway.
pub fn validate_remote_url(url: &str, allow_local: bool) -> Result<()> {
    if url.is_empty() || url.len() > MAX_URL_LEN {
        return Err(Error::InvalidRemote("empty or too long"));
    }
    if url.starts_with('-') {
        return Err(Error::InvalidRemote("begins with a dash"));
    }
    if url.bytes().any(|b| b <= 0x20 || b >= 0x7f) {
        return Err(Error::InvalidRemote(
            "contains whitespace, control or non-ASCII characters",
        ));
    }
    let lower = url.to_ascii_lowercase();
    if ["%0a", "%0d", "%00"].iter().any(|e| lower.contains(e)) {
        return Err(Error::InvalidRemote(
            "contains an encoded control character",
        ));
    }
    if let Some(rest) = url.strip_prefix("https://") {
        let authority = rest.split('/').next().unwrap_or("");
        if authority.contains('@') {
            return Err(Error::InvalidRemote("embeds credentials"));
        }
        if authority.is_empty() || authority.starts_with('-') {
            return Err(Error::InvalidRemote("has an invalid host"));
        }
        return Ok(());
    }
    if url.starts_with('/') {
        if !allow_local {
            return Err(Error::InvalidRemote("local paths are not enabled"));
        }
        if url.split('/').any(|part| part == "..") {
            return Err(Error::InvalidRemote("local path contains .."));
        }
        return Ok(());
    }
    Err(Error::InvalidRemote(
        "only https:// is accepted; ext::, fd::, ssh and other transports are refused",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_ids_must_be_full_lowercase_hex() {
        assert!(validate_commit_id(&"a".repeat(40)).is_ok());
        assert!(validate_commit_id(&"a".repeat(64)).is_ok());
        for bad in [
            "",
            "abc123",
            "HEAD",
            "--output=/tmp/x",
            "main",
            &"A".repeat(40),
            &"g".repeat(40),
            &"a".repeat(41),
        ] {
            assert!(validate_commit_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn bundle_refs_allowlist() {
        for good in [
            "HEAD",
            "refs/heads/main",
            "refs/heads/feature/a-b_c.d",
            "refs/tags/v1.0",
        ] {
            assert!(validate_bundle_ref(good).is_ok(), "{good}");
        }
        for bad in [
            "-evil",
            "--upload-pack=touch",
            "refs/heads/-evil",
            "refs/heads/a/-b",
            "refs/heads/",
            "refs/heads/a b",
            "refs/heads/a\nb",
            "refs/heads/a\0b",
            "refs/heads/../../config",
            "refs/heads/a..b",
            "refs/heads/.hidden",
            "refs/heads/a.lock",
            "refs/heads/a.",
            "refs/heads//a",
            "refs/heads/a@{1}",
            "refs/heads/a~1",
            "refs/heads/a^",
            "refs/heads/a:b",
            "refs/replace/abcd",
            "refs/quarantine/1/x",
            "refs/notes/x",
            "main",
            "",
        ] {
            assert!(validate_bundle_ref(bad).is_err(), "{bad:?}");
        }
        assert!(validate_bundle_ref(&format!("refs/heads/{}", "a".repeat(300))).is_err());
    }

    #[test]
    fn task_names_are_strict() {
        for good in ["a", "fix-123", "task_1.2", "0abc"] {
            assert!(validate_task_name(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "-x",
            "--force",
            ".x",
            "_x",
            "a/b",
            "../main",
            "a..b",
            "a.",
            "a.lock",
            "Fix",
            "a b",
            "a\n",
            "a:b",
            "+a",
            "a*",
            "é",
            &"a".repeat(65),
        ] {
            assert!(validate_task_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn remote_urls_are_allowlisted() {
        assert!(validate_remote_url("https://example.invalid/o/r.git", false).is_ok());
        assert!(validate_remote_url("/tmp/r.git", true).is_ok());
        for bad in [
            "ext::sh -c touch% /tmp/x",
            "ext::sh",
            "fd::3",
            "FD::3",
            "EXT::sh",
            "-oProxyCommand=x",
            "--upload-pack=x",
            "ssh://host/r",
            "ssh://-oProxyCommand=x/r",
            "git@github.example:o/r.git",
            "http://example.invalid/r",
            "git://example.invalid/r",
            "file:///tmp/r",
            "HTTPS://example.invalid/r",
            "https::x",
            "https://user:token@example.invalid/r",
            "https://token@example.invalid/r",
            "https://-x/r",
            "https:///r",
            "https://example.invalid/a b",
            "https://example.invalid/a\nb",
            "https://example.invalid/a%0ab",
            "/tmp/r.git",
            "relative/path",
            "",
        ] {
            assert!(validate_remote_url(bad, false).is_err(), "{bad:?}");
        }
        assert!(validate_remote_url("/tmp/../r.git", true).is_err());
        assert!(validate_remote_url("ext::sh", true).is_err());
    }

    #[test]
    fn remote_errors_do_not_echo_the_url() {
        let err = validate_remote_url("https://u:hunter2@example.invalid/r", false).unwrap_err();
        assert!(!err.to_string().contains("hunter2"));
    }
}
