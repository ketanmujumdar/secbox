//! The lines of one reviewed change, for a person to read.
//!
//! Git makes the comparison, with `git diff-tree -p` in the quarantine. That
//! repository is bare, so attributes in the hostile tree are not read, and
//! external drivers and text conversion are turned off. `--text` keeps a file
//! from being called binary by anything but the check made here, and this reads
//! Git's output strictly: a line it cannot place makes the file unreadable.
//!
//! A file that looks like it holds a secret is withheld whole, so its value is
//! never sent to a client. Both sides are checked: the baseline can hold a
//! secret too, and a context or removed line would show it. Withholding can be
//! triggered on purpose by planting a key-shaped string, so a client must call
//! such a file unread and never imply it was fine.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use crate::detect::{high_entropy_value, scan_line};
use crate::error::Result;
use crate::quarantine::Quarantine;
use crate::review::{FileChange, FlagKind, Review, EMPTY_TREE};
use crate::secretpath::{path_exclusion, ExclusionReason};

const MODE_NONE: &str = "000000";
const MODE_GITLINK: &str = "160000";
const NULL_OID: &str = "0000000000000000000000000000000000000000";
/// Bounds on what one comparison reads for patches.
#[derive(Clone, Debug)]
pub struct PatchLimits {
    /// Largest side of a change that is compared.
    pub max_blob_bytes: u64,
    /// Lines kept for one file. The counts still cover all of it.
    pub max_lines: usize,
}

impl Default for PatchLimits {
    fn default() -> Self {
        Self {
            max_blob_bytes: 1024 * 1024,
            max_lines: 5_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

/// One line of a patch. `text` is the file's exact bytes without the line end:
/// hostile until a client escapes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchLine {
    pub kind: LineKind,
    pub text: Vec<u8>,
    /// The last line of a side, with no line end after it.
    pub no_newline: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// One-based first line in the old file, or 0 when the hunk has none.
    pub old_start: u64,
    /// One-based first line in the new file, or 0 when the hunk has none.
    pub new_start: u64,
    pub lines: Vec<PatchLine>,
}

/// What a person can be shown of one change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Patch {
    Text {
        hunks: Vec<Hunk>,
        added: u64,
        removed: u64,
        /// A limit cut the lines, so `hunks` is not all of the change.
        truncated: bool,
    },
    /// It looks like it holds a secret, so nothing of it is shown.
    Withheld,
    Binary,
    TooLarge,
    Submodule,
    Unreadable,
}

impl Review {
    /// The lines of one change from [`Review::changes`], read from `quarantine`.
    /// `holds_secret` is asked about each side as a whole, so a caller can add
    /// values only it knows, such as the secrets given to this project.
    pub fn patch(
        &self,
        quarantine: &Quarantine,
        change: &FileChange,
        limits: &PatchLimits,
        holds_secret: &dyn Fn(&[u8]) -> bool,
    ) -> Result<Patch> {
        if self.path_holds_secret(change) {
            return Ok(Patch::Withheld);
        }
        if change.old_mode == MODE_GITLINK || change.new_mode == MODE_GITLINK {
            return Ok(Patch::Submodule);
        }
        let old = match read_side(quarantine, &change.old_mode, &change.old_oid, limits)? {
            Side::Blob(blob) => blob,
            Side::Stop(patch) => return Ok(patch),
        };
        let new = match read_side(quarantine, &change.new_mode, &change.new_oid, limits)? {
            Side::Blob(blob) => blob,
            Side::Stop(patch) => return Ok(patch),
        };
        // The content check comes first, so a file that is both is withheld.
        if has_secret(&old, holds_secret) || has_secret(&new, holds_secret) {
            return Ok(Patch::Withheld);
        }
        if old.contains(&0) || new.contains(&0) {
            return Ok(Patch::Binary);
        }
        let mut pathspec = b":(literal)".to_vec();
        pathspec.extend_from_slice(change.raw_path());
        let out = quarantine
            .git("diff-tree")
            .args([
                "diff-tree",
                "-r",
                "-p",
                "--text",
                "--unified=3",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "--ignore-submodules=none",
                self.base().unwrap_or(EMPTY_TREE),
                self.tip(),
                "--",
            ])
            .arg(OsStr::from_bytes(&pathspec))
            .max_stdout(limits.max_blob_bytes.saturating_mul(8))
            .run()?;
        if !out.success() {
            return Ok(Patch::Unreadable);
        }
        Ok(parse_unified(&out.stdout, limits.max_lines).unwrap_or(Patch::Unreadable))
    }

    /// The name alone says so, or the review found a secret at this path, in
    /// the end result or in any commit on the way to it.
    ///
    /// Editor and agent files are left to the content check. `mcp.json` and its
    /// like can make a tool run commands on the person's machine, so they are
    /// among the files most worth reading, and a token in one is still caught.
    fn path_holds_secret(&self, change: &FileChange) -> bool {
        let flagged = self.flags().iter().any(|flag| {
            matches!(flag.kind, FlagKind::LikelySecret(_) | FlagKind::EnvFile)
                && flag.path.as_deref() == Some(change.display_path.as_str())
        });
        let parts: Vec<&[u8]> = change.raw_path().split(|b| *b == b'/').collect();
        let by_name = !matches!(
            path_exclusion(&parts, true),
            None | Some(ExclusionReason::EditorOrAgentSecret | ExclusionReason::GitMetadata)
        );
        flagged || by_name
    }
}

enum Side {
    Blob(Vec<u8>),
    Stop(Patch),
}

fn read_side(
    quarantine: &Quarantine,
    mode: &str,
    oid: &str,
    limits: &PatchLimits,
) -> Result<Side> {
    if mode == MODE_NONE || oid == NULL_OID {
        return Ok(Side::Blob(Vec::new()));
    }
    let size = quarantine
        .git("cat-file")
        .args(["cat-file", "-s", oid])
        .run()?;
    if !size.success() {
        return Ok(Side::Stop(Patch::Unreadable));
    }
    let Ok(size) = size.stdout_trimmed().parse::<u64>() else {
        return Ok(Side::Stop(Patch::Unreadable));
    };
    if size > limits.max_blob_bytes {
        return Ok(Side::Stop(Patch::TooLarge));
    }
    let out = quarantine
        .git("cat-file")
        .args(["cat-file", "blob", oid])
        .max_stdout(limits.max_blob_bytes)
        .run()?;
    if !out.success() {
        return Ok(Side::Stop(Patch::Unreadable));
    }
    Ok(Side::Blob(out.stdout))
}

fn has_secret(blob: &[u8], holds_secret: &dyn Fn(&[u8]) -> bool) -> bool {
    holds_secret(blob)
        || blob
            .split(|b| *b == b'\n')
            .any(|line| !scan_line(line).is_empty() || high_entropy_value(line))
}

/// One `@@ -a,b +c,d @@` header: where each side starts and how many lines of
/// it follow. Git leaves out a count of 1. What comes after is not read.
fn parse_hunk_header(line: &[u8]) -> Option<(u64, u64, u64, u64)> {
    let line = std::str::from_utf8(line.strip_prefix(b"@@ ")?.split(|b| *b == b'@').next()?).ok()?;
    let mut parts = line.split_ascii_whitespace();
    let side = |part: Option<&str>, sign: char| -> Option<(u64, u64)> {
        let (start, count) = match part?.strip_prefix(sign)?.split_once(',') {
            Some((start, count)) => (start, count.parse().ok()?),
            None => (part?.strip_prefix(sign)?, 1),
        };
        Some((start.parse().ok()?, count))
    };
    let (old_start, old_count) = side(parts.next(), '-')?;
    let (new_start, new_count) = side(parts.next(), '+')?;
    Some((old_start, old_count, new_start, new_count))
}

/// Reads Git's patch for one path. The counts in each hunk header say exactly
/// which lines are content, so a file holding text that looks like a header or
/// a marker stays content. `None` when the output is not what Git writes.
fn parse_unified(out: &[u8], max_lines: usize) -> Option<Patch> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let (mut old_left, mut new_left) = (0u64, 0u64);
    let (mut added, mut removed) = (0u64, 0u64);
    let mut kept = 0usize;
    let mut truncated = false;
    let mut last_kept = false;

    for raw in out.split_inclusive(|b| *b == b'\n') {
        let line = raw.strip_suffix(b"\n").unwrap_or(raw);
        if line.starts_with(b"\\ ") {
            if last_kept {
                hunks.last_mut()?.lines.last_mut()?.no_newline = true;
            }
            continue;
        }
        if old_left == 0 && new_left == 0 {
            // A file's own header, or the next hunk. A type change has two of
            // each, one for the old entry and one for the new.
            if line.starts_with(b"@@ ") {
                let (old_start, old_count, new_start, new_count) = parse_hunk_header(line)?;
                old_left = old_count;
                new_left = new_count;
                last_kept = false;
                if kept < max_lines {
                    hunks.push(Hunk {
                        old_start: if old_count == 0 { 0 } else { old_start },
                        new_start: if new_count == 0 { 0 } else { new_start },
                        lines: Vec::new(),
                    });
                }
            }
            continue;
        }
        let (sign, text) = line.split_first()?;
        let kind = match sign {
            b' ' => {
                old_left = old_left.checked_sub(1)?;
                new_left = new_left.checked_sub(1)?;
                LineKind::Context
            }
            b'+' => {
                new_left = new_left.checked_sub(1)?;
                added += 1;
                LineKind::Added
            }
            b'-' => {
                old_left = old_left.checked_sub(1)?;
                removed += 1;
                LineKind::Removed
            }
            _ => return None,
        };
        last_kept = kept < max_lines;
        if last_kept {
            hunks.last_mut()?.lines.push(PatchLine {
                kind,
                text: text.to_vec(),
                no_newline: false,
            });
            kept += 1;
        } else {
            truncated = true;
        }
    }
    if old_left != 0 || new_left != 0 {
        return None;
    }
    Some(Patch::Text {
        hunks,
        added,
        removed,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n";

    fn parsed(body: &str) -> Patch {
        parse_unified(format!("{HEADER}{body}").as_bytes(), 100).unwrap()
    }

    fn signed(patch: &Patch) -> Vec<String> {
        let Patch::Text { hunks, .. } = patch else {
            panic!("{patch:?}");
        };
        hunks
            .iter()
            .flat_map(|h| &h.lines)
            .map(|l| {
                let sign = match l.kind {
                    LineKind::Context => ' ',
                    LineKind::Added => '+',
                    LineKind::Removed => '-',
                };
                format!("{sign}{}", String::from_utf8_lossy(&l.text))
            })
            .collect()
    }

    #[test]
    fn a_hunk_keeps_its_lines_numbers_and_counts() {
        let patch = parsed("@@ -2,3 +2,3 @@ fn name() {\n a\n-b\n+B\n c\n");
        assert_eq!(signed(&patch), [" a", "-b", "+B", " c"]);
        let Patch::Text { hunks, added, removed, truncated } = &patch else {
            panic!();
        };
        assert_eq!((*added, *removed, *truncated), (1, 1, false));
        assert_eq!((hunks[0].old_start, hunks[0].new_start), (2, 2));
    }

    #[test]
    fn an_added_or_deleted_file_has_no_start_on_the_empty_side() {
        let added = parsed("@@ -0,0 +1,2 @@\n+a\n+b\n");
        let Patch::Text { hunks, .. } = &added else {
            panic!();
        };
        assert_eq!((hunks[0].old_start, hunks[0].new_start), (0, 1));
        let gone = parsed("@@ -1 +0,0 @@\n-a\n");
        let Patch::Text { hunks, .. } = &gone else {
            panic!();
        };
        assert_eq!((hunks[0].old_start, hunks[0].new_start), (1, 0));
    }

    #[test]
    fn git_s_no_newline_marker_lands_on_the_right_line() {
        let patch = parsed("@@ -1 +1 @@\n-a\n+a\n\\ No newline at end of file\n");
        let Patch::Text { hunks, .. } = &patch else {
            panic!();
        };
        assert!(!hunks[0].lines[0].no_newline);
        assert!(hunks[0].lines[1].no_newline);
        assert_eq!(signed(&patch), ["-a", "+a"]);
    }

    /// A file can hold text shaped like Git's own output. It stays content.
    #[test]
    fn content_that_looks_like_a_header_or_marker_stays_content() {
        let body = "@@ -1,4 +1,4 @@\n @@ -9,9 +9,9 @@\n-diff --git a/x b/x\n+--- a/x\n \\ fake\n  +++ b/x\n";
        assert_eq!(
            signed(&parsed(body)),
            [" @@ -9,9 +9,9 @@", "-diff --git a/x b/x", "+--- a/x", " \\ fake", "  +++ b/x"]
        );
    }

    #[test]
    fn a_type_change_reads_as_its_two_parts() {
        let out = format!("{HEADER}@@ -1 +0,0 @@\n-old\n{HEADER}@@ -0,0 +1 @@\n+target\n");
        let patch = parse_unified(out.as_bytes(), 100).unwrap();
        assert_eq!(signed(&patch), ["-old", "+target"]);
    }

    #[test]
    fn only_a_mode_change_has_no_hunks() {
        let out = "diff --git a/f b/f\nold mode 100644\nnew mode 100755\n";
        let Patch::Text { hunks, added, removed, .. } = parse_unified(out.as_bytes(), 100).unwrap() else {
            panic!();
        };
        assert!(hunks.is_empty());
        assert_eq!((added, removed), (0, 0));
    }

    #[test]
    fn the_line_limit_cuts_the_lines_says_so_and_keeps_true_counts() {
        let lines: String = (0..50).map(|i| format!("+{i}\n")).collect();
        let out = format!("{HEADER}@@ -0,0 +1,50 @@\n{lines}");
        let Patch::Text { hunks, added, truncated, .. } = parse_unified(out.as_bytes(), 10).unwrap() else {
            panic!();
        };
        assert!(truncated);
        assert_eq!(added, 50);
        assert_eq!(hunks.iter().map(|h| h.lines.len()).sum::<usize>(), 10);
    }

    #[test]
    fn output_that_is_not_what_git_writes_is_refused() {
        for bad in [
            "@@ -1 +1 @@\n-a\n",
            "@@ -1 +1 @@\n?a\n+b\n",
            "@@ -1 +1 @@\n\n",
            "@@ nonsense @@\n",
            "@@ -x +1 @@\n+a\n",
            "@@ -0,0 +1 @@\n+a\n+b\n+c\n@@ bad\n",
        ] {
            assert!(parse_unified(bad.as_bytes(), 100).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn a_secret_on_either_side_or_of_the_callers_own_is_seen() {
        let none = |_: &[u8]| false;
        let key = format!("key = \"AKIA{}\"\n", "IOSFODNN7EXAMPLE");
        assert!(has_secret(key.as_bytes(), &none));
        let named = format!("password = \"{}\"\n", "a1b2c3d4e5f6g7h8i9j0k1l2");
        assert!(has_secret(named.as_bytes(), &none));
        assert!(!has_secret(b"let x = 1;\n", &none));
        assert!(has_secret(b"let x = 1;\n", &|blob: &[u8]| blob.starts_with(b"let")));
    }
}
