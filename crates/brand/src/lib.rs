//! The single source of truth for the product name.
//!
//! Other crates must use these constants instead of writing the name.
//! `scripts/rename.sh` changes the name, and the test below guards the rule.

pub const PRODUCT_NAME: &str = "secbox";
pub const CLI_BIN: &str = "secbox";
pub const DAEMON_BIN: &str = "secboxd";
pub const CONFIG_FILE: &str = "secbox.yaml";
pub const STATE_DIR_NAME: &str = "secbox";
/// The hidden state folder in the user's home.
pub const STATE_DIR: &str = ".secbox";
/// Joined to [`ENV_PREFIX`], this names the variable that replaces the home
/// folder holding [`STATE_DIR`]. The folder name itself cannot be overridden.
pub const HOME_ENV_SUFFIX: &str = "HOME";
/// Joined to [`ENV_PREFIX`], this names the variable giving the engine binary.
pub const ENGINE_BIN_ENV_SUFFIX: &str = "ENGINE_BIN";
pub const KEYCHAIN_SERVICE: &str = "secbox";
pub const ENV_PREFIX: &str = "SECBOX_";
pub const BRANCH_PREFIX: &str = "secbox/";
pub const SOCKET_FILE: &str = "secboxd.sock";

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    fn scan(dir: &Path, hits: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path, hits);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = fs::read_to_string(&path).unwrap().to_lowercase();
                if text.contains(super::PRODUCT_NAME) {
                    hits.push(path.display().to_string());
                }
            }
        }
    }

    #[test]
    fn rust_code_outside_brand_uses_constants() {
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let mut hits = Vec::new();
        for entry in fs::read_dir(crates).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() && path.file_name().unwrap() != "brand" {
                scan(&path, &mut hits);
            }
        }
        assert!(hits.is_empty(), "hardcoded product name in: {hits:?}");
    }
}
