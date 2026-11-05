//! File access limited to the project directory.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::Error;

/// What a project-relative path turns out to be.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    Missing,
    File(PathBuf),
    Dir(PathBuf),
    /// A FIFO, socket, device or something that could not be inspected.
    Other,
    /// Resolves outside the project, or is a broken or looping link.
    Escapes,
}

/// Resolves `rel` under `root`. This inspects metadata only, so following a
/// link here reads nothing from outside the project.
pub(crate) fn resolve_inside(root: &Path, rel: &Path) -> Result<Entry, Error> {
    let root = fs::canonicalize(root).map_err(|source| Error::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let candidate = root.join(rel);
    match fs::symlink_metadata(&candidate) {
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Entry::Missing),
        Err(err) if err.kind() == io::ErrorKind::NotADirectory => return Ok(Entry::Missing),
        Err(_) => return Ok(Entry::Other),
    }
    let Ok(real) = fs::canonicalize(&candidate) else {
        return Ok(Entry::Escapes);
    };
    if !real.starts_with(&root) {
        return Ok(Entry::Escapes);
    }
    let Ok(meta) = fs::symlink_metadata(&real) else {
        return Ok(Entry::Other);
    };
    Ok(if meta.is_file() {
        Entry::File(real)
    } else if meta.is_dir() {
        Entry::Dir(real)
    } else {
        Entry::Other
    })
}

/// True for `.env`, `.env.local`, `prod.env` and similar, in any case.
pub(crate) fn is_env_file_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    name == ".env" || name.starts_with(".env.") || name.ends_with(".env")
}

/// Reads a regular file inside the project, up to `limit` bytes. Environment
/// files are refused, including through a link.
pub(crate) fn read_inside(root: &Path, rel: &Path, limit: u64) -> Result<String, Error> {
    if is_env_file_name(rel) {
        return Err(Error::EnvFileRefused(rel.to_path_buf()));
    }
    let real = match resolve_inside(root, rel)? {
        Entry::File(real) => real,
        Entry::Escapes => return Err(Error::PathEscapes(rel.to_path_buf())),
        Entry::Missing => {
            return Err(Error::Io {
                path: rel.to_path_buf(),
                source: io::Error::from(io::ErrorKind::NotFound),
            })
        }
        Entry::Dir(_) | Entry::Other => return Err(Error::NotARegularFile(rel.to_path_buf())),
    };
    if is_env_file_name(&real) {
        return Err(Error::EnvFileRefused(rel.to_path_buf()));
    }
    let io_err = |source| Error::Io {
        path: rel.to_path_buf(),
        source,
    };
    let before = fs::symlink_metadata(&real).map_err(io_err)?;
    let file = fs::File::open(&real).map_err(io_err)?;
    let after = file.metadata().map_err(io_err)?;
    if !after.is_file() || !same_file(&before, &after) {
        return Err(Error::FileChanged(rel.to_path_buf()));
    }
    if after.len() > limit {
        return Err(Error::TooLarge {
            path: rel.to_path_buf(),
            limit,
        });
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(io_err)?;
    if bytes.len() as u64 > limit {
        return Err(Error::TooLarge {
            path: rel.to_path_buf(),
            limit,
        });
    }
    String::from_utf8(bytes).map_err(|_| Error::NotUtf8(rel.to_path_buf()))
}

#[cfg(unix)]
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.len() == b.len()
}
