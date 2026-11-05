//! Which names never enter a snapshot, decided from the path alone.
//!
//! The snapshot copies a trusted repository into a directory the VM can read, so
//! anything in it is readable by hostile code. Leaving a file out costs the VM a
//! file it can usually do without; copying a secret cannot be undone. Every rule
//! here therefore errs towards leaving the file out, and every exclusion is
//! reported so a person can see what happened.
//!
//! Matching rules, all deliberate:
//!
//! - Names are folded with [`fold_component`], so case, zero-width characters
//!   and lookalikes such as the Kelvin sign cannot hide a name.
//! - Trailing dots and spaces are then cut, because a checkout on a filesystem
//!   that drops them turns `.env.` back into `.env`.
//! - A component is also split on backslashes, so a single name holding what
//!   another system reads as a path cannot smuggle `.aws\credentials` past the
//!   pair rules.
//! - Directory rules match any component, so a whole subtree goes. Name rules
//!   match any component too: a *directory* called `id_rsa` or `.env` holds
//!   secrets just as surely as a file of that name, and the cost of being wrong
//!   is one reported exclusion.
//! - The only way back in is an example file, and only in the two families where
//!   examples are a real convention: `.env` and Terraform variables. The name
//!   must be exact, on a regular file, in the last position.

use serde::Serialize;

use crate::display::fold_component;

/// Why an entry is not in the snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReason {
    /// `.env`, `.env.*` or `*.env`, in any case, or anything inside such a
    /// directory. A plain example file is kept.
    EnvFile,
    /// A private key, a key store, or a directory that holds them.
    PrivateKey,
    /// Credentials for a cloud provider or its command-line tool.
    CloudCredentials,
    /// Credentials for a package registry, a daemon or another local tool,
    /// including shell and client history files.
    ToolCredentials,
    /// A password store, an encrypted blob, or a file whose name says it holds
    /// secrets.
    SecretStore,
    /// Infrastructure state or variables, which hold the values they applied.
    InfrastructureState,
    /// An editor or coding-agent file that commonly carries a token.
    EditorOrAgentSecret,
    /// A path component that would act as git metadata.
    GitMetadata,
    /// A symlink that leaves the snapshot or does not resolve inside it.
    EscapingSymlink,
}

use ExclusionReason as R;

/// Whole directories whose contents are credentials.
const SECRET_DIRS: &[(&str, ExclusionReason)] = &[
    (".ssh", R::PrivateKey),
    (".gnupg", R::PrivateKey),
    (".password-store", R::SecretStore),
    (".aws", R::CloudCredentials),
    (".azure", R::CloudCredentials),
    (".oci", R::CloudCredentials),
    (".kube", R::CloudCredentials),
    (".gcloud", R::CloudCredentials),
    (".terraform", R::InfrastructureState),
];

/// Two neighbouring components. The first is a directory that also holds
/// harmless files, so only the named child goes.
const SECRET_PAIRS: &[(&str, &str, ExclusionReason)] = &[
    (".config", "gcloud", R::CloudCredentials),
    (".docker", "config.json", R::ToolCredentials),
    (".composer", "auth.json", R::ToolCredentials),
    (".cargo", "credentials", R::ToolCredentials),
    (".cargo", "credentials.toml", R::ToolCredentials),
    (".gem", "credentials", R::ToolCredentials),
    (".bundle", "config", R::ToolCredentials),
    (".gradle", "gradle.properties", R::ToolCredentials),
    (".m2", "settings.xml", R::ToolCredentials),
    (".pip", "pip.conf", R::ToolCredentials),
    (".vscode", "sftp.json", R::EditorOrAgentSecret),
    (".vscode", "mcp.json", R::EditorOrAgentSecret),
    (".cursor", "mcp.json", R::EditorOrAgentSecret),
    (".idea", "datasources.local.xml", R::EditorOrAgentSecret),
    (".idea", "webservers.xml", R::EditorOrAgentSecret),
    (".claude", ".credentials.json", R::EditorOrAgentSecret),
];

/// Exact names, already folded and lowercase.
const SECRET_NAMES: &[(&str, ExclusionReason)] = &[
    // Private keys and identities.
    ("id_rsa", R::PrivateKey),
    ("id_dsa", R::PrivateKey),
    ("id_ecdsa", R::PrivateKey),
    ("id_ecdsa_sk", R::PrivateKey),
    ("id_ed25519", R::PrivateKey),
    ("id_ed25519_sk", R::PrivateKey),
    ("id_xmss", R::PrivateKey),
    ("secring.gpg", R::PrivateKey),
    ("secring.auto", R::PrivateKey),
    // Cloud and cluster credentials.
    ("application_default_credentials.json", R::CloudCredentials),
    ("legacy_credentials", R::CloudCredentials),
    ("credentials.db", R::CloudCredentials),
    ("access_tokens.db", R::CloudCredentials),
    ("gcloud-service-key.json", R::CloudCredentials),
    ("service-account.json", R::CloudCredentials),
    ("service_account.json", R::CloudCredentials),
    ("serviceaccount.json", R::CloudCredentials),
    ("client_secret.json", R::CloudCredentials),
    ("credentials.json", R::CloudCredentials),
    ("kubeconfig", R::CloudCredentials),
    ("kube_config", R::CloudCredentials),
    ("azureprofile.json", R::CloudCredentials),
    ("accesstokens.json", R::CloudCredentials),
    (".s3cfg", R::CloudCredentials),
    (".boto", R::CloudCredentials),
    // Package registries, daemons and other local tools.
    (".npmrc", R::ToolCredentials),
    (".yarnrc", R::ToolCredentials),
    (".pypirc", R::ToolCredentials),
    (".netrc", R::ToolCredentials),
    ("_netrc", R::ToolCredentials),
    (".dockercfg", R::ToolCredentials),
    (".git-credentials", R::ToolCredentials),
    (".pgpass", R::ToolCredentials),
    (".my.cnf", R::ToolCredentials),
    (".htpasswd", R::ToolCredentials),
    (".rclone.conf", R::ToolCredentials),
    ("rclone.conf", R::ToolCredentials),
    (".terraformrc", R::ToolCredentials),
    ("terraform.rc", R::ToolCredentials),
    ("credentials.tfrc.json", R::ToolCredentials),
    (".vault-token", R::ToolCredentials),
    (".ftpconfig", R::ToolCredentials),
    // History files. They hold whatever was typed, including flags with keys.
    (".bash_history", R::ToolCredentials),
    (".zsh_history", R::ToolCredentials),
    (".sh_history", R::ToolCredentials),
    (".ksh_history", R::ToolCredentials),
    (".python_history", R::ToolCredentials),
    (".node_repl_history", R::ToolCredentials),
    (".psql_history", R::ToolCredentials),
    (".mysql_history", R::ToolCredentials),
    (".rediscli_history", R::ToolCredentials),
    (".irb_history", R::ToolCredentials),
    // Files whose name says they hold secrets.
    (".secret", R::SecretStore),
    (".secrets", R::SecretStore),
    ("secret.json", R::SecretStore),
    ("secret.yml", R::SecretStore),
    ("secret.yaml", R::SecretStore),
    ("secrets.json", R::SecretStore),
    ("secrets.yml", R::SecretStore),
    ("secrets.yaml", R::SecretStore),
    ("secrets.toml", R::SecretStore),
    ("secrets.ini", R::SecretStore),
    ("secrets.cfg", R::SecretStore),
    ("vault_pass.txt", R::SecretStore),
    (".vault_pass", R::SecretStore),
    (".vault-password", R::SecretStore),
    // Editor and coding-agent files that carry tokens.
    (".mcp.json", R::EditorOrAgentSecret),
    ("mcp.json", R::EditorOrAgentSecret),
    (".claude.json", R::EditorOrAgentSecret),
    (".credentials.json", R::EditorOrAgentSecret),
    ("sftp.json", R::EditorOrAgentSecret),
    ("sftp-config.json", R::EditorOrAgentSecret),
];

/// Extensions. A name that is only the extension, such as `.pem`, matches too.
const SECRET_SUFFIXES: &[(&str, ExclusionReason)] = &[
    (".pem", R::PrivateKey),
    (".key", R::PrivateKey),
    (".keystore", R::PrivateKey),
    (".jks", R::PrivateKey),
    (".p12", R::PrivateKey),
    (".pfx", R::PrivateKey),
    (".pkcs12", R::PrivateKey),
    (".p8", R::PrivateKey),
    (".ppk", R::PrivateKey),
    (".kdbx", R::SecretStore),
    (".kdb", R::SecretStore),
    (".keychain", R::SecretStore),
    (".keychain-db", R::SecretStore),
    (".agilekeychain", R::SecretStore),
    (".opvault", R::SecretStore),
    (".psafe3", R::SecretStore),
    (".gpg", R::SecretStore),
    (".pgp", R::SecretStore),
    (".kubeconfig", R::CloudCredentials),
    (".tfstate", R::InfrastructureState),
    (".tfvars", R::InfrastructureState),
    (".tfvars.json", R::InfrastructureState),
];

/// Markers that a file is a copy of another file. A copy of a secret is a
/// secret, so they are stripped before the name is matched.
const BACKUP_SUFFIXES: &[&str] = &[
    ".bak", ".backup", ".old", ".orig", ".save", ".saved", ".copy", ".swp", ".swo", ".tmp", "~",
];

/// Words that mark a file as a placeholder rather than the real thing.
const EXAMPLE_WORDS: &[&str] = &["example", "sample", "template", "dist", "defaults"];

/// One path component, folded for matching.
struct Part {
    /// Case and invisible characters folded away.
    folded: String,
    /// `folded` without trailing dots and spaces. Rules match this.
    name: String,
}

/// Fold the components, splitting each on backslashes as well.
fn parts_of(components: &[&[u8]]) -> Vec<Part> {
    let mut parts = Vec::with_capacity(components.len());
    for raw in components {
        for piece in raw.split(|b| *b == b'\\') {
            let folded = fold_component(piece);
            let name = folded.trim_end_matches(['.', ' ', '\t']).to_string();
            parts.push(Part { folded, name });
        }
    }
    parts
}

/// True for `.env`, `.env.anything` and `anything.env`.
fn is_env_name(name: &str) -> bool {
    name == ".env" || name.starts_with(".env.") || name.ends_with(".env")
}

/// True for Terraform variable files, which hold the values that were applied.
fn is_tfvars_name(name: &str) -> bool {
    name.ends_with(".tfvars") || name.ends_with(".tfvars.json")
}

/// The reason one component earns on its own, looking through any number of
/// backup markers: `server.key.bak` and `id_rsa~` are still keys.
fn name_reason(name: &str) -> Option<ExclusionReason> {
    let mut rest = name;
    for _ in 0..8 {
        if let Some(reason) = plain_name_reason(rest) {
            return Some(reason);
        }
        rest = BACKUP_SUFFIXES
            .iter()
            .find_map(|suffix| rest.strip_suffix(suffix))?;
    }
    None
}

/// The reason one exact name earns.
fn plain_name_reason(name: &str) -> Option<ExclusionReason> {
    if name.is_empty() {
        return None;
    }
    if is_env_name(name) {
        return Some(R::EnvFile);
    }
    if let Some((_, reason)) = SECRET_NAMES.iter().find(|(known, _)| *known == name) {
        return Some(*reason);
    }
    if let Some((_, reason)) = SECRET_SUFFIXES
        .iter()
        .find(|(suffix, _)| name.ends_with(suffix))
    {
        return Some(*reason);
    }
    // `terraform.tfstate.backup` and other state files with a suffix of their own.
    if name.contains(".tfstate.") {
        return Some(R::InfrastructureState);
    }
    None
}

/// A placeholder that is safe to copy: an exact example name of the `.env` or
/// Terraform variable families. A private key is never an example.
fn is_allowed_example(part: &Part) -> bool {
    // Trailing dots or spaces mean the name is not the one it pretends to be.
    if part.folded != part.name {
        return false;
    }
    let name = part.name.as_str();
    EXAMPLE_WORDS.iter().any(|word| {
        let as_suffix = name
            .strip_suffix(word)
            .and_then(|rest| rest.strip_suffix('.'))
            .is_some_and(|stem| is_env_name(stem) || is_tfvars_name(stem));
        // `example.env`, the other common spelling.
        let as_prefix = name
            .strip_prefix(word)
            .and_then(|rest| rest.strip_prefix('.'))
            .is_some_and(|stem| stem == "env");
        as_suffix || as_prefix
    })
}

fn is_git_metadata_name(name: &str) -> bool {
    name == ".git" || name == "git~1"
}

/// Why this path is left out of the snapshot, if it is.
///
/// `components` are the `/`-separated parts of the path, as raw bytes.
/// `is_file` must be true only for a regular file, because the example
/// exception is for regular files alone.
pub(crate) fn path_exclusion(components: &[&[u8]], is_file: bool) -> Option<ExclusionReason> {
    let parts = parts_of(components);
    let last = parts.len().saturating_sub(1);
    for (index, part) in parts.iter().enumerate() {
        if is_git_metadata_name(&part.name) {
            return Some(R::GitMetadata);
        }
        if let Some((_, reason)) = SECRET_DIRS.iter().find(|(dir, _)| *dir == part.name) {
            return Some(*reason);
        }
        if let Some(next) = parts.get(index + 1) {
            if let Some((_, _, reason)) = SECRET_PAIRS
                .iter()
                .find(|(dir, child, _)| *dir == part.name && *child == next.name)
            {
                return Some(*reason);
            }
        }
        if let Some(reason) = name_reason(&part.name) {
            let placeholder = index == last && is_file && is_allowed_example(part);
            if !placeholder {
                return Some(reason);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Split like the snapshot does, then ask for the reason.
    fn reason(path: &str, is_file: bool) -> Option<ExclusionReason> {
        let bytes = path.as_bytes();
        let parts: Vec<&[u8]> = bytes.split(|b| *b == b'/').collect();
        path_exclusion(&parts, is_file)
    }

    fn file(path: &str) -> Option<ExclusionReason> {
        reason(path, true)
    }

    #[test]
    fn env_variants_are_excluded_in_any_case_and_any_directory() {
        for path in [
            ".env",
            ".ENV",
            ".env.local",
            ".env.production",
            ".env.production.local",
            ".env.development.local",
            ".Env.Staging",
            "deep/er/.env.test",
            "prod.env",
            "config/local.env",
            ".env.vault",
            ".e\u{200c}nv",
            ".env.",
            ".env ",
            ".ENV...  ",
        ] {
            assert_eq!(file(path), Some(R::EnvFile), "{path}");
        }
    }

    #[test]
    fn example_env_files_are_kept() {
        for path in [
            ".env.example",
            ".env.sample",
            ".env.template",
            ".env.dist",
            ".env.defaults",
            "deep/.ENV.EXAMPLE",
            ".env.local.example",
            // The real test project commits this one, two levels of suffix
            // deep and not at the repository root.
            "infra/.env.production.example",
            "example.env",
            "sample.env",
            "terraform.tfvars.example",
            "prod.tfvars.sample",
        ] {
            assert_eq!(file(path), None, "{path}");
        }
    }

    #[test]
    fn an_example_name_only_saves_a_regular_file_in_the_last_position() {
        // A directory or symlink of that name is not an example.
        assert_eq!(reason(".env.example", false), Some(R::EnvFile));
        assert_eq!(file(".env.example/inner"), Some(R::EnvFile));
        // Trailing dots and spaces mean it is a different file on this disk.
        assert_eq!(file(".env.example."), Some(R::EnvFile));
        assert_eq!(file(".env.example "), Some(R::EnvFile));
        // The example word must be the whole last part.
        assert_eq!(file(".env.example.local"), Some(R::EnvFile));
        assert_eq!(file(".env.examples"), Some(R::EnvFile));
        // The exception is for the two families that have the convention. Other
        // names simply fall outside the rules, which is a decision, not an
        // exception: `id_rsa.example` is not a key file name at all. Content
        // scanning is what catches a real key hidden in one.
        assert_eq!(file("id_rsa.example"), None);
        assert_eq!(file(".npmrc.example"), None);
    }

    #[test]
    fn a_backup_copy_of_a_secret_is_still_a_secret() {
        for path in [
            "id_rsa.bak",
            "certs/server.key.orig",
            "certs/server.key.bak.old",
            ".npmrc~",
            ".aws/credentials.save",
            "terraform.tfvars.backup",
            ".env.bak",
        ] {
            assert!(file(path).is_some(), "{path}");
        }
        for path in ["notes.old", "config.json.bak", "main.rs~", "~"] {
            assert_eq!(file(path), None, "{path}");
        }
    }

    #[test]
    fn ordinary_names_are_kept() {
        for path in [
            "src/main.rs",
            "README.md",
            ".envrc",
            ".environment",
            "env/bin/activate",
            "environment.yml",
            "package.json",
            "deploy/key_helper.go",
            "docs/monkey.txt",
            "certs/server.crt",
            "certs/server.pub",
            "id_rsa.pub",
            ".gitignore",
            ".github/workflows/ci.yml",
            "config/credentials.yml.enc",
            ".cargo/config.toml",
            ".docker/daemon.json",
            "tests/fixtures/secrets.rs",
            ".secrets.baseline",
        ] {
            assert_eq!(file(path), None, "{path}");
        }
    }

    #[test]
    fn private_keys_and_stores_are_excluded() {
        for path in [
            "id_rsa",
            "id_ed25519",
            "deep/ID_ECDSA",
            "certs/server.pem",
            "certs/server.key",
            "app.p12",
            "app.pfx",
            "release.keystore",
            "release.jks",
            "key.p8",
            "putty.ppk",
            ".ssh/config",
            ".ssh/id_rsa",
            ".gnupg/secring.gpg",
            "vault.kdbx",
            "login.keychain-db",
            "backup.gpg",
            ".password-store/work/aws.gpg",
        ] {
            assert!(
                matches!(file(path), Some(R::PrivateKey) | Some(R::SecretStore)),
                "{path}"
            );
        }
    }

    #[test]
    fn cloud_and_tool_credentials_are_excluded() {
        let table: &[(&str, ExclusionReason)] = &[
            (".aws/credentials", R::CloudCredentials),
            (".aws/config", R::CloudCredentials),
            ("home/.AWS/Credentials", R::CloudCredentials),
            (".config/gcloud/credentials.db", R::CloudCredentials),
            (".gcloud/access_tokens.db", R::CloudCredentials),
            ("application_default_credentials.json", R::CloudCredentials),
            (".kube/config", R::CloudCredentials),
            ("ops/dev.kubeconfig", R::CloudCredentials),
            ("kubeconfig", R::CloudCredentials),
            (".azure/azureProfile.json", R::CloudCredentials),
            (".npmrc", R::ToolCredentials),
            ("packages/ui/.npmrc", R::ToolCredentials),
            (".pypirc", R::ToolCredentials),
            (".netrc", R::ToolCredentials),
            (".docker/config.json", R::ToolCredentials),
            (".git-credentials", R::ToolCredentials),
            (".pgpass", R::ToolCredentials),
            (".bash_history", R::ToolCredentials),
            (".cargo/credentials.toml", R::ToolCredentials),
            ("terraform.tfvars", R::InfrastructureState),
            ("prod.tfvars.json", R::InfrastructureState),
            ("terraform.tfstate", R::InfrastructureState),
            ("terraform.tfstate.backup", R::InfrastructureState),
            (".terraform/terraform.tfstate", R::InfrastructureState),
            (".vscode/sftp.json", R::EditorOrAgentSecret),
            (".cursor/mcp.json", R::EditorOrAgentSecret),
            (".mcp.json", R::EditorOrAgentSecret),
            (".claude/.credentials.json", R::EditorOrAgentSecret),
            (".idea/dataSources.local.xml", R::EditorOrAgentSecret),
            ("secrets.yaml", R::SecretStore),
            ("k8s/secrets.yml", R::SecretStore),
        ];
        for (path, expected) in table {
            assert_eq!(file(path), Some(*expected), "{path}");
        }
    }

    #[test]
    fn a_directory_named_like_a_secret_takes_its_whole_subtree() {
        for path in [
            ".env/inner",
            ".env.local/deeper/file.txt",
            "id_rsa/notes.txt",
            "certs/server.pem/data",
            ".aws/sub/dir/file",
            ".ssh/keys/work",
        ] {
            assert!(file(path).is_some(), "{path}");
            assert!(reason(path, false).is_some(), "{path}");
        }
    }

    #[test]
    fn separators_inside_one_component_do_not_hide_a_pair() {
        // A single file name that another system reads as a path.
        let one = [&b".aws\\credentials"[..]];
        assert_eq!(path_exclusion(&one, true), Some(R::CloudCredentials));
        let nested = [&b"home"[..], b".docker\\config.json"];
        assert_eq!(path_exclusion(&nested, true), Some(R::ToolCredentials));
    }

    #[test]
    fn git_metadata_still_wins() {
        assert_eq!(file(".git/config"), Some(R::GitMetadata));
        assert_eq!(file("sub/.GIT/hooks/pre-push"), Some(R::GitMetadata));
        assert_eq!(file("GIT~1/config"), Some(R::GitMetadata));
    }

    #[test]
    fn empty_and_odd_components_do_not_match() {
        assert_eq!(path_exclusion(&[&b""[..]], true), None);
        assert_eq!(path_exclusion(&[&b"..."[..]], true), None);
        assert_eq!(path_exclusion(&[&b"\xff\xfe"[..]], true), None);
    }
}
