//! The lines of one reviewed change, for a person to read.
//!
//! As in the review itself, git only hands over the blobs. Attributes in the
//! hostile tree can steer git's own diff, for example `-diff` makes a text file
//! look binary, so the line comparison is done here.
//!
//! A file that looks like it holds a secret is withheld whole, so its value is
//! never sent to a client. Both sides are checked: the baseline can hold a
//! secret too, and a context or removed line would show it. Withholding can be
//! triggered on purpose by planting a key-shaped string, so a client must call
//! such a file unread and never imply it was fine.

use crate::detect::{high_entropy_value, scan_line};
use crate::error::Result;
use crate::quarantine::Quarantine;
use crate::review::{FileChange, FlagKind, Review};
use crate::secretpath::{path_exclusion, ExclusionReason};

const MODE_NONE: &str = "000000";
const MODE_GITLINK: &str = "160000";
const NULL_OID: &str = "0000000000000000000000000000000000000000";
/// Lines of unchanged text kept around a change.
const CONTEXT: usize = 3;
/// How far the comparison looks for the shortest edit. Past this it reports the
/// differing middle as replaced, which is correct but longer.
const MAX_EDIT_DISTANCE: usize = 1000;
/// More lines than this in the differing middle skip the search altogether.
const MAX_SEARCH_LINES: usize = 40_000;

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
        Ok(compare(&old, &new, limits.max_lines))
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

/// Lines with their line ends, so `a` and `a\n` differ, as they do on disk.
fn split_lines(blob: &[u8]) -> Vec<&[u8]> {
    blob.split_inclusive(|b| *b == b'\n').collect()
}

/// Which lines of `old` and `new` pair up, as an edit script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edit {
    Keep,
    Remove,
    Add,
}

/// Myers' shortest edit script over line ids, with a bound on the search.
/// Returns `None` past the bound.
fn shortest_edit(old: &[u32], new: &[u32]) -> Option<Vec<Edit>> {
    let (n, m) = (old.len() as isize, new.len() as isize);
    let limit = MAX_EDIT_DISTANCE.min(old.len() + new.len()) as isize;
    let offset = limit + 1;
    let mut furthest = vec![0isize; (2 * offset + 1) as usize];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    for d in 0..=limit {
        trace.push(furthest.clone());
        for k in (-d..=d).step_by(2) {
            let at = (k + offset) as usize;
            let down = k == -d || (k != d && furthest[at - 1] < furthest[at + 1]);
            let mut x = if down {
                furthest[at + 1]
            } else {
                furthest[at - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && old[x as usize] == new[y as usize] {
                x += 1;
                y += 1;
            }
            furthest[at] = x;
            if x >= n && y >= m {
                return Some(backtrack(&trace, offset, n, m, d));
            }
        }
    }
    None
}

fn backtrack(trace: &[Vec<isize>], offset: isize, n: isize, m: isize, last: isize) -> Vec<Edit> {
    let (mut x, mut y) = (n, m);
    let mut edits = Vec::new();
    for d in (0..=last).rev() {
        let furthest = &trace[d as usize];
        let k = x - y;
        let at = (k + offset) as usize;
        let down = k == -d || (k != d && furthest[at - 1] < furthest[at + 1]);
        let prev_k = if down { k + 1 } else { k - 1 };
        let prev_x = furthest[(prev_k + offset) as usize];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            edits.push(Edit::Keep);
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            edits.push(if x == prev_x { Edit::Add } else { Edit::Remove });
        }
        x = prev_x;
        y = prev_y;
    }
    edits.reverse();
    edits
}

/// The whole edit script. The shared start and end are taken off first, since
/// most changes are small and a hostile file should not get a costly search.
fn edit_script(old: &[&[u8]], new: &[&[u8]]) -> Vec<Edit> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old_mid = &old[prefix..old.len() - suffix];
    let new_mid = &new[prefix..new.len() - suffix];

    let mut ids = std::collections::HashMap::new();
    let mut id_of = |line: &[u8]| -> u32 {
        let next = ids.len() as u32;
        *ids.entry(line.to_vec()).or_insert(next)
    };
    let searchable = old_mid.len() + new_mid.len() <= MAX_SEARCH_LINES;
    let middle = searchable
        .then(|| {
            let old_ids: Vec<u32> = old_mid.iter().map(|line| id_of(line)).collect();
            let new_ids: Vec<u32> = new_mid.iter().map(|line| id_of(line)).collect();
            shortest_edit(&old_ids, &new_ids)
        })
        .flatten();

    let mut edits = vec![Edit::Keep; prefix];
    match middle {
        Some(found) => edits.extend(found),
        None => {
            edits.extend(std::iter::repeat_n(Edit::Remove, old_mid.len()));
            edits.extend(std::iter::repeat_n(Edit::Add, new_mid.len()));
        }
    }
    edits.extend(std::iter::repeat_n(Edit::Keep, suffix));
    edits
}

fn patch_line(kind: LineKind, line: &[u8]) -> PatchLine {
    let text = line.strip_suffix(b"\n");
    PatchLine {
        kind,
        text: text.unwrap_or(line).to_vec(),
        no_newline: text.is_none(),
    }
}

/// Groups an edit script into hunks with [`CONTEXT`] lines around each change.
fn compare(old: &[u8], new: &[u8], max_lines: usize) -> Patch {
    let old_lines = split_lines(old);
    let new_lines = split_lines(new);
    let edits = edit_script(&old_lines, &new_lines);
    let added = edits.iter().filter(|e| **e == Edit::Add).count() as u64;
    let removed = edits.iter().filter(|e| **e == Edit::Remove).count() as u64;

    // Which positions in the script are shown: changes and their context.
    let mut shown = vec![false; edits.len()];
    for (at, edit) in edits.iter().enumerate() {
        if *edit != Edit::Keep {
            let end = (at + CONTEXT + 1).min(edits.len());
            shown[at.saturating_sub(CONTEXT)..end].fill(true);
        }
    }

    let mut hunks: Vec<Hunk> = Vec::new();
    let (mut at_old, mut at_new) = (0usize, 0usize);
    let mut kept = 0usize;
    let mut truncated = false;
    let mut open = false;
    for (at, edit) in edits.iter().enumerate() {
        if shown[at] && !truncated {
            if kept >= max_lines {
                truncated = true;
            } else {
                if !open {
                    hunks.push(Hunk {
                        old_start: if at_old < old_lines.len() { at_old as u64 + 1 } else { 0 },
                        new_start: if at_new < new_lines.len() { at_new as u64 + 1 } else { 0 },
                        lines: Vec::new(),
                    });
                    open = true;
                }
                let line = match edit {
                    Edit::Keep => patch_line(LineKind::Context, new_lines[at_new]),
                    Edit::Remove => patch_line(LineKind::Removed, old_lines[at_old]),
                    Edit::Add => patch_line(LineKind::Added, new_lines[at_new]),
                };
                if let Some(hunk) = hunks.last_mut() {
                    hunk.lines.push(line);
                }
                kept += 1;
            }
        } else {
            open = false;
        }
        match edit {
            Edit::Keep => {
                at_old += 1;
                at_new += 1;
            }
            Edit::Remove => at_old += 1,
            Edit::Add => at_new += 1,
        }
    }
    Patch::Text {
        hunks,
        added,
        removed,
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(patch: &Patch) -> Vec<String> {
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

    /// Applying the script must rebuild both sides, whatever the input.
    fn check_script(old: &[u8], new: &[u8]) {
        let (old_lines, new_lines) = (split_lines(old), split_lines(new));
        let (mut a, mut b) = (Vec::new(), Vec::new());
        let (mut i, mut j) = (0, 0);
        for edit in edit_script(&old_lines, &new_lines) {
            match edit {
                Edit::Keep => {
                    assert_eq!(old_lines[i], new_lines[j]);
                    a.push(old_lines[i]);
                    b.push(new_lines[j]);
                    i += 1;
                    j += 1;
                }
                Edit::Remove => {
                    a.push(old_lines[i]);
                    i += 1;
                }
                Edit::Add => {
                    b.push(new_lines[j]);
                    j += 1;
                }
            }
        }
        assert_eq!(a.concat(), old);
        assert_eq!(b.concat(), new);
    }

    #[test]
    fn an_edit_script_rebuilds_both_sides() {
        let cases: &[(&[u8], &[u8])] = &[
            (b"", b""),
            (b"", b"a\nb\n"),
            (b"a\nb\n", b""),
            (b"a\nb\nc\n", b"a\nx\nc\n"),
            (b"a\nb\nc\nd\n", b"b\nd\ne\n"),
            (b"a\nb", b"a\nb\n"),
            (b"x\nx\nx\n", b"x\nx\n"),
            (b"1\n2\n3\n4\n5\n", b"5\n4\n3\n2\n1\n"),
        ];
        for (old, new) in cases {
            check_script(old, new);
        }
    }

    /// Many small shuffled inputs, from a fixed seed so a failure repeats.
    #[test]
    fn an_edit_script_rebuilds_both_sides_of_arbitrary_input() {
        let mut seed = 0x2545_f491_u32;
        let mut next = move |bound: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed % bound
        };
        for _ in 0..2000 {
            let mut side = || -> Vec<u8> {
                let lines = next(12);
                (0..lines)
                    .flat_map(|_| [b'a' + next(4) as u8, b'\n'])
                    .collect()
            };
            let (old, new) = (side(), side());
            check_script(&old, &new);
        }
    }

    #[test]
    fn a_small_change_keeps_three_lines_around_it() {
        let old = b"1\n2\n3\n4\n5\n6\n7\n8\n9\n";
        let new = b"1\n2\n3\n4\nfive\n6\n7\n8\n9\n";
        let patch = compare(old, new, 100);
        assert_eq!(
            text_of(&patch),
            [" 2", " 3", " 4", "-5", "+five", " 6", " 7", " 8"]
        );
        let Patch::Text { hunks, added, removed, truncated } = &patch else {
            panic!();
        };
        assert_eq!((*added, *removed, *truncated), (1, 1, false));
        assert_eq!(hunks.len(), 1);
        assert_eq!((hunks[0].old_start, hunks[0].new_start), (2, 2));
    }

    #[test]
    fn changes_far_apart_get_their_own_hunks_with_true_line_numbers() {
        let old: String = (1..=30).map(|i| format!("{i}\n")).collect();
        let new = old.replace("5\n", "five\n").replacen("25\n", "", 1);
        let patch = compare(old.as_bytes(), new.as_bytes(), 1000);
        let Patch::Text { hunks, .. } = &patch else {
            panic!();
        };
        assert!(hunks.len() >= 2, "{hunks:?}");
        assert_eq!(hunks[0].old_start, 2);
    }

    #[test]
    fn an_added_or_deleted_file_shows_every_line() {
        let added = compare(b"", b"a\nb\n", 100);
        assert_eq!(text_of(&added), ["+a", "+b"]);
        let Patch::Text { hunks, .. } = &added else {
            panic!();
        };
        assert_eq!((hunks[0].old_start, hunks[0].new_start), (0, 1));
        assert_eq!(text_of(&compare(b"a\n", b"", 100)), ["-a"]);
    }

    #[test]
    fn a_missing_last_line_end_is_a_change_and_is_marked() {
        let patch = compare(b"a\n", b"a", 100);
        assert_eq!(text_of(&patch), ["-a", "+a"]);
        let Patch::Text { hunks, .. } = &patch else {
            panic!();
        };
        assert!(!hunks[0].lines[0].no_newline);
        assert!(hunks[0].lines[1].no_newline);
    }

    #[test]
    fn identical_sides_have_no_hunks() {
        let Patch::Text { hunks, added, removed, truncated } = compare(b"a\n", b"a\n", 100) else {
            panic!();
        };
        assert!(hunks.is_empty());
        assert_eq!((added, removed, truncated), (0, 0, false));
    }

    #[test]
    fn the_line_limit_cuts_the_lines_says_so_and_keeps_true_counts() {
        let new: String = (0..50).map(|i| format!("{i}\n")).collect();
        let Patch::Text { hunks, added, truncated, .. } = compare(b"", new.as_bytes(), 10) else {
            panic!();
        };
        assert!(truncated);
        assert_eq!(added, 50);
        assert_eq!(hunks.iter().map(|h| h.lines.len()).sum::<usize>(), 10);
    }

    /// A file built to make the search slow still ends, with a correct result.
    #[test]
    fn a_change_past_the_search_bound_is_shown_as_replaced() {
        let old: String = (0..5000).map(|i| format!("old{i}\n")).collect();
        let new: String = (0..5000).map(|i| format!("new{i}\n")).collect();
        check_script(old.as_bytes(), new.as_bytes());
        let Patch::Text { added, removed, .. } = compare(old.as_bytes(), new.as_bytes(), 10) else {
            panic!();
        };
        assert_eq!((added, removed), (5000, 5000));
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
