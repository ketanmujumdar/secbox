//! Starting the editor.
//!
//! The program is found on the PATH, or named by a user-level variable, and
//! then started directly with the folder as one argument. There is no shell
//! anywhere in this, so nothing in a folder name is ever interpreted, and no
//! argument comes from a project file.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::paths::Editor;

/// The user-level variable that names the editor program, for an editor that
/// is not on the PATH. A project cannot set it.
pub fn editor_bin_var() -> String {
    format!("{}EDITOR_BIN", brand::ENV_PREFIX)
}

/// Finds the program. The variable wins, and must be an absolute path.
pub fn resolve(editor: Editor, env: &dyn Fn(&str) -> Option<OsString>) -> Result<PathBuf, String> {
    let var = editor_bin_var();
    if let Some(value) = env(&var).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            return Err(format!("{var} must be an absolute path"));
        }
        if !runnable(&path) {
            return Err(format!("{var} does not name a program that can be run"));
        }
        return Ok(path);
    }
    let name = editor.command();
    let path = env("PATH").ok_or_else(|| format!("{name} was not found: PATH is not set"))?;
    for dir in std::env::split_paths(&path) {
        if !dir.is_absolute() {
            continue;
        }
        let candidate = dir.join(name);
        if runnable(&candidate) {
            return Ok(candidate);
        }
    }
    Err(format!("{name} was not found on the PATH"))
}

fn runnable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Starts the editor on one folder and does not wait for it. The folder is a
/// canonical absolute path, so it can never look like an option.
pub fn start(program: &Path, folder: &Path) -> Result<(), String> {
    if !folder.is_absolute() {
        return Err("the folder must be an absolute path".to_string());
    }
    Command::new(program)
        .arg(folder)
        .stdin(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not start it: {}", e.kind()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn the_variable_must_be_an_absolute_path_to_a_program() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("code");
        fs::write(&program, b"#!/bin/sh\n").unwrap();
        fn with(value: String) -> impl Fn(&str) -> Option<OsString> {
            move |key: &str| (key == editor_bin_var()).then(|| OsString::from(value.clone()))
        }
        assert!(resolve(Editor::Code, &with("code".to_string())).is_err());
        let full = program.display().to_string();
        assert!(resolve(Editor::Code, &with(full.clone())).is_err());
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(resolve(Editor::Code, &with(full)).unwrap(), program);
    }

    #[test]
    fn the_path_is_searched_and_relative_entries_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("cursor");
        fs::write(&program, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        let value = format!(".:{}", dir.path().display());
        let env = move |key: &str| (key == "PATH").then(|| OsString::from(value.clone()));
        assert_eq!(resolve(Editor::Cursor, &env).unwrap(), program);
        let empty = |_: &str| None;
        assert!(resolve(Editor::Cursor, &empty).is_err());
    }

    #[test]
    fn a_relative_folder_is_refused() {
        assert!(start(Path::new("/usr/bin/true"), Path::new("relative")).is_err());
    }
}
