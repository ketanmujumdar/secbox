//! A strict parser for the bundle header, run before git sees the file.
//!
//! This exists because git is not strict enough here: `git bundle list-heads`
//! on 2.50.1 happily lists refs named `-evil` and `--upload-pack=touch`.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use crate::error::{Error, Result};
use crate::validate::{validate_bundle_ref, validate_commit_id};

const MAX_HEADER_BYTES: u64 = 1024 * 1024;
const MAX_REFS: usize = 1024;
const MAX_PREREQUISITES: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct BundleHeader {
    pub refs: Vec<(String, String)>,
    pub prerequisites: Vec<String>,
}

pub(crate) fn read_header(path: &Path) -> Result<BundleHeader> {
    let file = File::open(path)?;
    parse_header(BufReader::new(file.take(MAX_HEADER_BYTES)))
}

pub(crate) fn parse_header(mut reader: impl BufRead) -> Result<BundleHeader> {
    let mut next_line = || -> Result<Option<String>> {
        let mut raw = Vec::new();
        if reader.read_until(b'\n', &mut raw)? == 0 {
            return Ok(None);
        }
        if raw.pop() != Some(b'\n') {
            return Err(Error::BundleMalformed("header is truncated or too long"));
        }
        // Keep bad bytes as a rejected ref rather than a generic failure.
        Ok(Some(String::from_utf8_lossy(&raw).into_owned()))
    };

    let signature = next_line()?.ok_or(Error::BundleMalformed("empty file"))?;
    let v3 = match signature.as_str() {
        "# v2 git bundle" => false,
        "# v3 git bundle" => true,
        _ => return Err(Error::BundleMalformed("unknown bundle signature")),
    };

    let mut refs: Vec<(String, String)> = Vec::new();
    let mut prerequisites = Vec::new();
    let mut folded = HashSet::new();
    loop {
        let line = next_line()?.ok_or(Error::BundleMalformed("header never ends"))?;
        if line.is_empty() {
            break;
        }
        if let Some(capability) = line.strip_prefix('@') {
            // A filtered bundle has missing objects, and sha256 does not match
            // the quarantine. Refuse any capability not known to be harmless.
            if !v3 || capability != "object-format=sha1" {
                return Err(Error::BundleMalformed("unsupported bundle capability"));
            }
        } else if let Some(rest) = line.strip_prefix('-') {
            let oid = rest.split(' ').next().unwrap_or("");
            validate_oid(oid)?;
            prerequisites.push(oid.to_string());
            if prerequisites.len() > MAX_PREREQUISITES {
                return Err(Error::BundleMalformed("too many prerequisites"));
            }
        } else {
            let (oid, name) = line
                .split_once(' ')
                .ok_or(Error::BundleMalformed("ref line has no name"))?;
            validate_oid(oid)?;
            validate_bundle_ref(name)?;
            // Refs are files, so these two would be one ref on a case-insensitive
            // filesystem and would silently overwrite each other.
            if !folded.insert(name.to_ascii_lowercase()) {
                return Err(Error::BadRefName(crate::display::escape_for_display(
                    name.as_bytes(),
                )));
            }
            refs.push((oid.to_string(), name.to_string()));
            if refs.len() > MAX_REFS {
                return Err(Error::BundleMalformed("too many refs"));
            }
        }
    }
    if refs.is_empty() {
        return Err(Error::BundleMalformed("bundle has no refs"));
    }
    Ok(BundleHeader {
        refs,
        prerequisites,
    })
}

/// The quarantine is sha1, so an object id in a header must be 40 hex digits.
fn validate_oid(oid: &str) -> Result<()> {
    if oid.len() != 40 || validate_commit_id(oid).is_err() {
        return Err(Error::BundleMalformed("bad object id in header"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<BundleHeader> {
        parse_header(text.as_bytes())
    }

    fn oid() -> String {
        "a".repeat(40)
    }

    #[test]
    fn parses_v2_and_v3() {
        let v2 = format!(
            "# v2 git bundle\n-{0} base commit\n{0} refs/heads/main\n{0} HEAD\n\nPACK",
            oid()
        );
        let header = parse(&v2).unwrap();
        assert_eq!(header.prerequisites, [oid()]);
        assert_eq!(header.refs.len(), 2);
        let v3 = format!(
            "# v3 git bundle\n@object-format=sha1\n{} refs/heads/main\n\nPACK",
            oid()
        );
        assert_eq!(parse(&v3).unwrap().refs.len(), 1);
    }

    #[test]
    fn rejects_bad_structure() {
        for bad in [
            String::new(),
            "not a bundle\n".to_string(),
            "# v9 git bundle\n\n".to_string(),
            "# v2 git bundle\n\n".to_string(),
            "# v2 git bundle\n".to_string(),
            format!("# v2 git bundle\n{} refs/heads/a", oid()),
            format!("# v2 git bundle\n{}\n\n", oid()),
            "# v2 git bundle\nabc refs/heads/a\n\n".to_string(),
            format!(
                "# v2 git bundle\n@object-format=sha1\n{} refs/heads/a\n\n",
                oid()
            ),
            format!(
                "# v3 git bundle\n@filter=blob:none\n{} refs/heads/a\n\n",
                oid()
            ),
            format!(
                "# v3 git bundle\n@object-format=sha256\n{} refs/heads/a\n\n",
                oid()
            ),
            format!("# v2 git bundle\n-xyz\n{} refs/heads/a\n\n", oid()),
        ] {
            assert!(parse(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn rejects_hostile_refs_before_git_runs() {
        for name in [
            "-evil",
            "--upload-pack=touch",
            "refs/heads/-evil",
            "refs/replace/x",
        ] {
            let text = format!("# v2 git bundle\n{} {name}\n\nPACK", oid());
            assert!(matches!(parse(&text), Err(Error::BadRefName(_))), "{name}");
        }
    }

    #[test]
    fn rejects_refs_colliding_by_case() {
        let text = format!(
            "# v2 git bundle\n{0} refs/heads/Main\n{0} refs/heads/main\n\n",
            oid()
        );
        assert!(matches!(parse(&text), Err(Error::BadRefName(_))));
    }

    #[test]
    fn bad_ref_error_is_escaped() {
        let text = format!("# v2 git bundle\n{} refs/heads/\x1b[31mred\n\n", oid());
        let message = parse(&text).unwrap_err().to_string();
        assert!(!message.contains('\x1b'), "{message}");
    }

    #[test]
    fn unterminated_large_header_is_rejected() {
        let huge = format!("# v2 git bundle\n{}", "a".repeat(2_000_000));
        let reader = BufReader::new(huge.as_bytes().take(MAX_HEADER_BYTES));
        assert!(parse_header(reader).is_err());
    }
}
