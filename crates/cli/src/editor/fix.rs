//! Changing user settings, with a backup first.
//!
//! Only a file under the user's own editor folder is ever written. Nothing in
//! a project folder is touched, and the caller must have consent before
//! calling [`set`]. A settings file that is a link is refused rather than
//! replaced, so a dotfiles setup is never quietly broken.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::jsonc::{self, Source};
use super::prefs::{now_unix, write_mode, write_private, BACKUP_DIR};
use crate::state::{ensure_state, StatePaths};

/// Copies the current file into the state folder and returns where it went.
pub fn backup(state: &StatePaths, settings: &Path, tag: &str) -> Result<PathBuf, String> {
    let text = current(settings)?;
    ensure_state(state)?;
    let dir = state.state.join(BACKUP_DIR);
    match fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err("the backup folder is not a real directory".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&dir)
                .map_err(|e| format!("cannot create the backup folder: {}", e.kind()))?;
        }
        Err(e) => return Err(format!("cannot inspect the backup folder: {}", e.kind())),
    }
    let at = dir.join(format!("{tag}-settings-{}.json", now_unix()));
    write_private(&at, text.as_bytes())?;
    Ok(at)
}

/// Sets one top-level setting, keeping the rest of the file byte for byte.
/// The result is parsed again before it is written, so a file is never left
/// holding something the editor cannot read.
pub fn set(settings: &Path, key: &str, value: &str) -> Result<(), String> {
    let text = current(settings)?;
    let mode = fs::metadata(settings)
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0o600);
    let updated = jsonc::set_top_level(&text, key, value)?;
    let parsed = jsonc::parse(&updated).map_err(|e| format!("the result would not parse: {e}"))?;
    let wanted: serde_json::Value =
        serde_json::from_str(value).map_err(|_| "the new value is not JSON".to_string())?;
    if parsed.get(key) != Some(&wanted) {
        return Err("the result would not hold the new value".to_string());
    }
    write_mode(settings, updated.as_bytes(), mode)
}

/// Reads the file as it is now, refusing anything that is not a plain file.
fn current(settings: &Path) -> Result<String, String> {
    if fs::symlink_metadata(settings).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("it is a symbolic link, so it was left alone".to_string());
    }
    match jsonc::read(settings) {
        Source::Text(text) => Ok(text),
        Source::Missing => Err("it is not there any more".to_string()),
        Source::NotRegular => Err("it is not a regular file".to_string()),
        Source::TooLarge => Err("it is too large to rewrite safely".to_string()),
        Source::Unreadable(reason) => Err(format!("it could not be read: {reason}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(text: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, text).unwrap();
        (dir, path)
    }

    #[test]
    fn a_fix_keeps_comments_and_trailing_commas() {
        let before = "{\n  // keep me\n  \"editor.fontSize\": 13,\n  \"task.allowAutomaticTasks\": \"on\",\n}\n";
        let (_dir, path) = temp(before);
        set(&path, "task.allowAutomaticTasks", "\"off\"").unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert!(after.contains("// keep me"), "{after}");
        assert!(after.contains("\"editor.fontSize\": 13,"), "{after}");
        assert!(
            after.contains("\"task.allowAutomaticTasks\": \"off\","),
            "{after}"
        );
        assert!(!after.contains("\"on\""), "{after}");
        // The trailing comma before the brace is still there.
        assert!(after.trim_end().ends_with(",\n}"), "{after}");
    }

    #[test]
    fn a_missing_setting_is_added_to_an_object_of_any_shape() {
        for (before, expect) in [
            ("{}", "true"),
            ("{\n}\n", "true"),
            ("{\n  \"a\": 1\n}\n", "true"),
            ("// only a comment\n", "true"),
            ("", "true"),
        ] {
            let (_dir, path) = temp(before);
            set(&path, "security.workspace.trust.enabled", expect).unwrap();
            let after = fs::read_to_string(&path).unwrap();
            let parsed = jsonc::parse(&after).unwrap();
            assert_eq!(
                parsed.get("security.workspace.trust.enabled"),
                Some(&serde_json::Value::Bool(true)),
                "{before:?} became {after:?}"
            );
            if before.contains("\"a\"") {
                assert_eq!(parsed.get("a"), Some(&serde_json::json!(1)));
            }
        }
    }

    #[test]
    fn a_link_or_a_folder_is_never_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.json");
        fs::write(&real, "{}").unwrap();
        let link = dir.path().join("settings.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(set(&link, "a", "1").is_err());
        assert_eq!(fs::read_to_string(&real).unwrap(), "{}");

        let folder = dir.path().join("folder.json");
        fs::create_dir(&folder).unwrap();
        assert!(set(&folder, "a", "1").is_err());
    }

    #[test]
    fn the_file_keeps_its_permissions() {
        let (_dir, path) = temp("{\"a\": 1}");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        set(&path, "a", "2").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
    }

    #[test]
    fn a_backup_holds_the_original_bytes() {
        let home = tempfile::tempdir().unwrap();
        let state = StatePaths {
            state: home.path().join(brand::STATE_DIR),
            overridden: true,
        };
        let (_dir, path) = temp("{\n  // first\n  \"a\": 1\n}\n");
        let at = backup(&state, &path, "vscode").unwrap();
        assert_eq!(
            fs::read_to_string(&at).unwrap(),
            "{\n  // first\n  \"a\": 1\n}\n"
        );
        let mode = fs::metadata(&at).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
