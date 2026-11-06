//! A minimal strict reader for the tar format `git archive` writes.
//!
//! Only regular files, directories, symlinks and pax headers are accepted. Hard
//! links, devices and FIFOs are errors. Writing this here, instead of running
//! the system `tar`, means excluded files never reach the destination at all.

use std::io::{self, Read};

use crate::error::{Error, Result};

const BLOCK: usize = 512;
const MAX_PAX_BYTES: u64 = 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EntryKind {
    File { executable: bool },
    Directory,
    Symlink { target: Vec<u8> },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub path: Vec<u8>,
    pub kind: EntryKind,
    pub size: u64,
}

fn trimmed(field: &[u8]) -> &[u8] {
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    &field[..end]
}

fn parse_octal(field: &[u8]) -> Result<u64> {
    let mut value: u64 = 0;
    let mut digits = 0;
    for byte in field {
        match byte {
            b'0'..=b'7' => {
                value = value
                    .checked_mul(8)
                    .and_then(|v| v.checked_add(u64::from(byte - b'0')))
                    .ok_or(Error::SnapshotFormat("number overflow"))?;
                digits += 1;
            }
            b' ' | 0 if digits > 0 => break,
            b' ' | 0 => {}
            _ => return Err(Error::SnapshotFormat("bad octal field")),
        }
    }
    Ok(value)
}

fn checksum_ok(header: &[u8; BLOCK]) -> Result<bool> {
    let stored = parse_octal(&header[148..156])?;
    let sum: u64 = header
        .iter()
        .enumerate()
        .map(|(i, b)| {
            if (148..156).contains(&i) {
                u64::from(b' ')
            } else {
                u64::from(*b)
            }
        })
        .sum();
    Ok(sum == stored)
}

#[derive(Default)]
struct PaxOverrides {
    path: Option<Vec<u8>>,
    linkpath: Option<Vec<u8>>,
    size: Option<u64>,
}

/// Parse `<len> <key>=<value>\n` records, where `len` counts the whole record.
fn parse_pax(mut body: &[u8]) -> Result<PaxOverrides> {
    let malformed = Error::SnapshotFormat;
    let mut out = PaxOverrides::default();
    while !body.is_empty() {
        let space = body
            .iter()
            .position(|b| *b == b' ')
            .ok_or(malformed("pax record has no length"))?;
        let len: usize = std::str::from_utf8(&body[..space])
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or(malformed("bad pax length"))?;
        if len <= space + 1 || len > body.len() || body[len - 1] != b'\n' {
            return Err(malformed("bad pax record"));
        }
        let record = &body[space + 1..len - 1];
        let equals = record
            .iter()
            .position(|b| *b == b'=')
            .ok_or(malformed("pax record has no key"))?;
        let (key, value) = (&record[..equals], &record[equals + 1..]);
        match key {
            b"path" => out.path = Some(value.to_vec()),
            b"linkpath" => out.linkpath = Some(value.to_vec()),
            b"size" => {
                out.size = Some(
                    std::str::from_utf8(value)
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .ok_or(malformed("bad pax size"))?,
                );
            }
            _ => {}
        }
        body = &body[len..];
    }
    Ok(out)
}

fn read_block(reader: &mut impl Read, block: &mut [u8; BLOCK]) -> Result<()> {
    reader.read_exact(block).map_err(|err| {
        if err.kind() == io::ErrorKind::UnexpectedEof {
            Error::SnapshotFormat("archive is truncated")
        } else {
            Error::Io(err)
        }
    })
}

fn drain(reader: &mut impl Read, bytes: u64) -> Result<()> {
    let copied = io::copy(&mut reader.by_ref().take(bytes), &mut io::sink())?;
    if copied != bytes {
        return Err(Error::SnapshotFormat("archive is truncated"));
    }
    Ok(())
}

fn padding(size: u64) -> u64 {
    (BLOCK as u64 - size % BLOCK as u64) % BLOCK as u64
}

/// Walk the archive, calling `visit` with each entry and a reader for its body.
/// Requires the end-of-archive marker, so a truncated stream is an error.
pub(crate) fn walk<R: Read>(
    mut reader: R,
    mut visit: impl FnMut(Entry, &mut dyn Read) -> Result<()>,
) -> Result<()> {
    let mut block = [0u8; BLOCK];
    let mut pending = PaxOverrides::default();
    loop {
        read_block(&mut reader, &mut block)?;
        if block.iter().all(|b| *b == 0) {
            return Ok(());
        }
        if !checksum_ok(&block)? {
            return Err(Error::SnapshotFormat("bad header checksum"));
        }
        if &block[257..262] != b"ustar" {
            return Err(Error::SnapshotFormat("not a ustar header"));
        }
        let typeflag = block[156];
        let header_size = parse_octal(&block[124..136])?;

        match typeflag {
            b'g' | b'x' => {
                if header_size > MAX_PAX_BYTES {
                    return Err(Error::SnapshotFormat("pax header too large"));
                }
                let mut body = Vec::with_capacity(header_size as usize);
                let got = reader.by_ref().take(header_size).read_to_end(&mut body)?;
                if got as u64 != header_size {
                    return Err(Error::SnapshotFormat("archive is truncated"));
                }
                drain(&mut reader, padding(header_size))?;
                if typeflag == b'x' {
                    pending = parse_pax(&body)?;
                }
                continue;
            }
            b'0' | 0 | b'5' | b'2' => {}
            _ => return Err(Error::SnapshotFormat("unsupported entry type")),
        }

        let overrides = std::mem::take(&mut pending);
        let size = overrides.size.unwrap_or(header_size);
        let mut path = match overrides.path {
            Some(path) => path,
            None => {
                let name = trimmed(&block[0..100]);
                let prefix = trimmed(&block[345..500]);
                if prefix.is_empty() {
                    name.to_vec()
                } else {
                    [prefix, b"/", name].concat()
                }
            }
        };
        if path.contains(&0) {
            return Err(Error::SnapshotFormat("path contains NUL"));
        }
        let kind = match typeflag {
            b'5' => {
                if path.last() == Some(&b'/') {
                    path.pop();
                }
                EntryKind::Directory
            }
            b'2' => EntryKind::Symlink {
                target: overrides
                    .linkpath
                    .unwrap_or_else(|| trimmed(&block[157..257]).to_vec()),
            },
            _ => {
                let mode = parse_octal(&block[100..108])?;
                EntryKind::File {
                    executable: mode & 0o100 != 0,
                }
            }
        };

        let body_size = match kind {
            EntryKind::File { .. } => size,
            _ => 0,
        };
        let mut body = reader.by_ref().take(body_size);
        visit(
            Entry {
                path,
                kind,
                size: body_size,
            },
            &mut body,
        )?;
        let unread = body.limit();
        drain(&mut reader, unread)?;
        drain(&mut reader, padding(body_size))?;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Build one header block, with a valid checksum.
    pub(crate) fn header(name: &[u8], typeflag: u8, size: u64, link: &[u8]) -> [u8; BLOCK] {
        let mut block = [0u8; BLOCK];
        block[..name.len()].copy_from_slice(name);
        block[100..108].copy_from_slice(b"0000644\0");
        block[108..116].copy_from_slice(b"0000000\0");
        block[116..124].copy_from_slice(b"0000000\0");
        block[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        block[136..148].copy_from_slice(b"00000000000\0");
        block[156] = typeflag;
        block[157..157 + link.len()].copy_from_slice(link);
        block[257..265].copy_from_slice(b"ustar\x0000");
        block[148..156].copy_from_slice(b"        ");
        let sum: u32 = block.iter().map(|b| u32::from(*b)).sum();
        block[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        block
    }

    pub(crate) fn file_entry(name: &[u8], body: &[u8]) -> Vec<u8> {
        let mut out = header(name, b'0', body.len() as u64, b"").to_vec();
        out.extend_from_slice(body);
        out.resize(out.len() + padding(body.len() as u64) as usize, 0);
        out
    }

    fn end() -> Vec<u8> {
        vec![0u8; BLOCK * 2]
    }

    fn collect(data: &[u8]) -> Result<Vec<(Entry, Vec<u8>)>> {
        let mut seen = Vec::new();
        walk(data, |entry, body| {
            let mut bytes = Vec::new();
            body.read_to_end(&mut bytes)?;
            seen.push((entry, bytes));
            Ok(())
        })?;
        Ok(seen)
    }

    #[test]
    fn reads_files_dirs_and_symlinks() {
        let mut data = file_entry(b"a.txt", b"hello");
        data.extend_from_slice(&header(b"dir/", b'5', 0, b""));
        data.extend_from_slice(&header(b"link", b'2', 0, b"a.txt"));
        data.extend(end());
        let seen = collect(&data).unwrap();
        assert_eq!(seen[0].0.path, b"a.txt");
        assert_eq!(seen[0].1, b"hello");
        assert_eq!(seen[1].0.path, b"dir");
        assert_eq!(seen[1].0.kind, EntryKind::Directory);
        assert_eq!(
            seen[2].0.kind,
            EntryKind::Symlink {
                target: b"a.txt".to_vec()
            }
        );
    }

    #[test]
    fn pax_overrides_apply_to_the_next_entry_only() {
        let record = |key: &str, value: &str| {
            let body = format!(" {key}={value}\n");
            let mut len = body.len();
            while format!("{len}{body}").len() != len {
                len = format!("{len}{body}").len();
            }
            format!("{len}{body}")
        };
        let pax = record("path", "long/name");
        let mut data = header(b"x", b'x', pax.len() as u64, b"").to_vec();
        data.extend_from_slice(pax.as_bytes());
        data.resize(data.len() + padding(pax.len() as u64) as usize, 0);
        data.extend(file_entry(b"short", b"1"));
        data.extend(file_entry(b"next", b"2"));
        data.extend(end());
        let seen = collect(&data).unwrap();
        assert_eq!(seen[0].0.path, b"long/name");
        assert_eq!(seen[1].0.path, b"next");
    }

    #[test]
    fn rejects_dangerous_types_and_damage() {
        for typeflag in *b"13467LKS" {
            let mut data = header(b"x", typeflag, 0, b"/etc/passwd").to_vec();
            data.extend(end());
            assert!(collect(&data).is_err(), "type {}", typeflag as char);
        }
        let mut bad_sum = file_entry(b"a", b"x");
        bad_sum[0] = b'b';
        bad_sum.extend(end());
        assert!(collect(&bad_sum).is_err());

        let mut no_magic = header(b"a", b'0', 0, b"");
        no_magic[257..262].copy_from_slice(b"xxxxx");
        no_magic[148..156].copy_from_slice(b"        ");
        let sum: u32 = no_magic.iter().map(|b| u32::from(*b)).sum();
        no_magic[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        let mut data = no_magic.to_vec();
        data.extend(end());
        assert!(collect(&data).is_err());

        assert!(collect(&file_entry(b"a", b"x")).is_err(), "no end marker");
        let cut = file_entry(b"a", &[b'x'; 1000]);
        assert!(collect(&cut[..700]).is_err(), "cut inside a body");
    }

    #[test]
    fn rejects_bad_pax_records() {
        for bad in ["", "x", "9 path", "2 a=b\n", "999 path=a\n", "5 a\n"] {
            let parsed = parse_pax(bad.as_bytes());
            assert!(bad.is_empty() || parsed.is_err(), "{bad:?}");
        }
    }
}
