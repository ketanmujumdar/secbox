//! Finds what kind of project a directory holds, from file names alone. Only
//! `Package.swift` is ever opened. Links that leave the project are skipped.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::safe_fs::{read_inside, resolve_inside, Entry};
use crate::{clean, Error};

/// Compose file names, in order of preference.
pub const COMPOSE_FILES: [&str; 4] = [
    "docker-compose.yml",
    "docker-compose.yaml",
    "compose.yml",
    "compose.yaml",
];

/// Files that Compose merges into the main file without being asked to.
pub const COMPOSE_OVERRIDE_FILES: [&str; 4] = [
    "docker-compose.override.yml",
    "docker-compose.override.yaml",
    "compose.override.yml",
    "compose.override.yaml",
];

const DEVCONTAINER_FILES: [&str; 2] = [".devcontainer/devcontainer.json", ".devcontainer.json"];

/// Manifest file, ecosystem and suggested base image, in order of preference.
/// The user approves any setup generated from these suggestions.
const MANIFESTS: [(&str, &str, &str); 17] = [
    ("package.json", "node", "node:22-bookworm-slim"),
    ("go.mod", "go", "golang:1.24-bookworm"),
    ("pyproject.toml", "python", "python:3.12-slim-bookworm"),
    ("requirements.txt", "python", "python:3.12-slim-bookworm"),
    ("Pipfile", "python", "python:3.12-slim-bookworm"),
    ("setup.py", "python", "python:3.12-slim-bookworm"),
    ("Cargo.toml", "rust", "rust:1-slim-bookworm"),
    ("Gemfile", "ruby", "ruby:3.3-slim-bookworm"),
    ("pom.xml", "java", "eclipse-temurin:21-jdk"),
    ("build.gradle", "java", "eclipse-temurin:21-jdk"),
    ("build.gradle.kts", "java", "eclipse-temurin:21-jdk"),
    ("composer.json", "php", "php:8.3-cli-bookworm"),
    ("mix.exs", "elixir", "elixir:1.18-slim"),
    ("deno.json", "deno", "denoland/deno:2"),
    ("deno.jsonc", "deno", "denoland/deno:2"),
    ("pubspec.yaml", "dart", "dart:stable"),
    ("Package.swift", "swift", "swift:6-bookworm"),
];

const MAX_DIR_ENTRIES: usize = 20_000;
const MAX_PACKAGE_SWIFT_BYTES: u64 = 256 * 1024;

/// Platforms in `Package.swift` that have no Linux build. `.macOS(` is left
/// out on purpose, because server-side Swift packages commonly declare it and
/// still build on Linux.
const APPLE_ONLY_PLATFORMS: [&str; 5] = [
    ".iOS(",
    ".tvOS(",
    ".watchOS(",
    ".visionOS(",
    ".macCatalyst(",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ProjectKind {
    Compose {
        file: PathBuf,
        /// Override files next to it. Compose merges these automatically.
        overrides: Vec<PathBuf>,
    },
    Dockerfile {
        file: PathBuf,
    },
    DevContainer {
        file: PathBuf,
    },
    Manifest {
        file: PathBuf,
        ecosystem: String,
        suggested_image: String,
    },
    /// The project needs macOS, so it cannot be protected in a Linux VM.
    Unsupported {
        marker: PathBuf,
        reason: String,
    },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Detection {
    pub kind: ProjectKind,
    /// Things worth showing the user, such as skipped links. Safe to print.
    pub notes: Vec<String>,
}

/// Detects the project kind. The first match wins: compose file, Dockerfile,
/// dev container, macOS-only marker, manifest, then unknown.
///
/// A macOS marker comes before manifests, because a `package.json` next to an
/// Xcode project does not make the Xcode build safe to run on Linux. An
/// explicit container setup still wins, with a note about the marker.
pub fn detect(root: &Path) -> Result<Detection, Error> {
    let mut notes = Vec::new();
    let mac_marker = find_macos_marker(root, &mut notes);

    let kind = if let Some(kind) = find_compose(root, &mut notes)? {
        kind
    } else if let Some(file) = first_file(root, &["Dockerfile"], &mut notes)? {
        ProjectKind::Dockerfile { file }
    } else if let Some(file) = first_file(root, &DEVCONTAINER_FILES, &mut notes)? {
        ProjectKind::DevContainer { file }
    } else if let Some(marker) = mac_marker.clone() {
        let reason = format!(
            "{} needs macOS tooling, which cannot run in a Linux VM, so this project cannot be protected",
            clean(&marker.display().to_string())
        );
        ProjectKind::Unsupported { marker, reason }
    } else if let Some(kind) = find_manifest(root, &mut notes)? {
        kind
    } else {
        ProjectKind::Unknown
    };

    if let (Some(marker), false) = (&mac_marker, matches!(kind, ProjectKind::Unsupported { .. })) {
        notes.push(format!(
            "{} is also present: any macOS builds in this project will not run in the VM",
            clean(&marker.display().to_string())
        ));
    }
    Ok(Detection { kind, notes })
}

fn find_compose(root: &Path, notes: &mut Vec<String>) -> Result<Option<ProjectKind>, Error> {
    let Some(file) = first_file(root, &COMPOSE_FILES, notes)? else {
        return Ok(None);
    };
    for other in COMPOSE_FILES {
        if Path::new(other) != file && resolve_inside(root, Path::new(other))? != Entry::Missing {
            notes.push(format!(
                "{other} is also present and was not chosen as the compose file"
            ));
        }
    }
    let mut overrides = Vec::new();
    for name in COMPOSE_OVERRIDE_FILES {
        match resolve_inside(root, Path::new(name))? {
            Entry::Missing => {}
            Entry::File(_) => overrides.push(PathBuf::from(name)),
            _ => notes.push(format!(
                "{name} exists but is a link out of the project or not a regular file"
            )),
        }
    }
    Ok(Some(ProjectKind::Compose { file, overrides }))
}

fn find_manifest(root: &Path, notes: &mut Vec<String>) -> Result<Option<ProjectKind>, Error> {
    for (name, ecosystem, image) in MANIFESTS {
        if let Some(file) = first_file(root, &[name], notes)? {
            return Ok(Some(ProjectKind::Manifest {
                file,
                ecosystem: ecosystem.to_string(),
                suggested_image: image.to_string(),
            }));
        }
    }
    Ok(None)
}

fn first_file(
    root: &Path,
    names: &[&str],
    notes: &mut Vec<String>,
) -> Result<Option<PathBuf>, Error> {
    for name in names {
        match resolve_inside(root, Path::new(name))? {
            Entry::Missing => {}
            Entry::File(_) => return Ok(Some(PathBuf::from(name))),
            Entry::Escapes => notes.push(format!(
                "skipped {name}: it is a broken link or a link out of the project"
            )),
            Entry::Dir(_) | Entry::Other => {
                notes.push(format!("skipped {name}: it is not a regular file"));
            }
        }
    }
    Ok(None)
}

fn find_macos_marker(root: &Path, notes: &mut Vec<String>) -> Option<PathBuf> {
    let mut found = None;
    if let Ok(entries) = fs::read_dir(root) {
        for (index, entry) in entries.flatten().enumerate() {
            if index >= MAX_DIR_ENTRIES {
                notes.push(format!(
                    "stopped looking for Xcode projects after {MAX_DIR_ENTRIES} directory entries"
                ));
                break;
            }
            let path = entry.path();
            let is_xcode = path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                e.eq_ignore_ascii_case("xcodeproj") || e.eq_ignore_ascii_case("xcworkspace")
            });
            // file_type does not follow links, so a linked bundle is ignored.
            if is_xcode && entry.file_type().is_ok_and(|t| t.is_dir()) {
                let name = PathBuf::from(entry.file_name());
                // Directory order varies, so keep the smallest name.
                if found.as_ref().is_none_or(|current| name < *current) {
                    found = Some(name);
                }
            }
        }
    }
    found.or_else(|| package_swift_marker(root, notes))
}

fn package_swift_marker(root: &Path, notes: &mut Vec<String>) -> Option<PathBuf> {
    let rel = Path::new("Package.swift");
    if !matches!(resolve_inside(root, rel), Ok(Entry::File(_))) {
        return None;
    }
    match read_inside(root, rel, MAX_PACKAGE_SWIFT_BYTES) {
        Ok(text) => {
            let apple_only =
                !text.contains(".linux") && APPLE_ONLY_PLATFORMS.iter().any(|p| text.contains(p));
            apple_only.then(|| rel.to_path_buf())
        }
        Err(err) => {
            notes.push(clean(&format!("could not check Package.swift: {err}")));
            None
        }
    }
}
