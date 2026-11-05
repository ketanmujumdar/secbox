use std::fs;
use std::path::{Path, PathBuf};

use projectcheck::detect::{detect, ProjectKind};
use tempfile::TempDir;

fn project(files: &[&str]) -> TempDir {
    let dir = TempDir::new().unwrap();
    for file in files {
        let path = dir.path().join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "").unwrap();
    }
    dir
}

fn kind(dir: &TempDir) -> ProjectKind {
    detect(dir.path()).unwrap().kind
}

#[test]
fn each_compose_name_is_found() {
    for name in [
        "docker-compose.yml",
        "docker-compose.yaml",
        "compose.yml",
        "compose.yaml",
    ] {
        let dir = project(&[name]);
        assert_eq!(
            kind(&dir),
            ProjectKind::Compose {
                file: PathBuf::from(name),
                overrides: Vec::new()
            }
        );
    }
}

#[test]
fn compose_beats_everything_else() {
    let dir = project(&[
        "compose.yaml",
        "Dockerfile",
        "package.json",
        ".devcontainer.json",
    ]);
    assert!(matches!(kind(&dir), ProjectKind::Compose { .. }));
}

#[test]
fn dockerfile_beats_devcontainer_and_manifests() {
    let dir = project(&["Dockerfile", ".devcontainer.json", "go.mod"]);
    assert_eq!(
        kind(&dir),
        ProjectKind::Dockerfile {
            file: "Dockerfile".into()
        }
    );
}

#[test]
fn both_devcontainer_locations_are_found() {
    for name in [".devcontainer/devcontainer.json", ".devcontainer.json"] {
        let dir = project(&[name, "package.json"]);
        assert_eq!(kind(&dir), ProjectKind::DevContainer { file: name.into() });
    }
}

#[test]
fn manifests_suggest_a_base_image() {
    for (name, ecosystem) in [
        ("package.json", "node"),
        ("go.mod", "go"),
        ("pyproject.toml", "python"),
        ("requirements.txt", "python"),
        ("Cargo.toml", "rust"),
    ] {
        let dir = project(&[name]);
        match kind(&dir) {
            ProjectKind::Manifest {
                file,
                ecosystem: found,
                suggested_image,
            } => {
                assert_eq!(file, Path::new(name));
                assert_eq!(found, ecosystem);
                assert!(!suggested_image.is_empty());
            }
            other => panic!("{name}: {other:?}"),
        }
    }
}

#[test]
fn empty_directory_is_unknown() {
    assert_eq!(kind(&project(&[])), ProjectKind::Unknown);
}

#[test]
fn missing_root_is_an_error() {
    let dir = TempDir::new().unwrap();
    assert!(detect(&dir.path().join("absent")).is_err());
}

#[test]
fn override_files_are_listed() {
    let dir = project(&["compose.yaml", "compose.override.yaml"]);
    assert_eq!(
        kind(&dir),
        ProjectKind::Compose {
            file: "compose.yaml".into(),
            overrides: vec!["compose.override.yaml".into()],
        }
    );
}

#[test]
fn xcode_projects_are_unsupported() {
    for name in ["App.xcodeproj", "App.xcworkspace"] {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(name)).unwrap();
        match kind(&dir) {
            ProjectKind::Unsupported { marker, reason } => {
                assert_eq!(marker, Path::new(name));
                assert!(reason.contains("macOS"));
            }
            other => panic!("{name}: {other:?}"),
        }
    }
}

#[test]
fn xcode_marker_comes_before_manifests_but_after_container_setups() {
    let dir = project(&["package.json"]);
    fs::create_dir(dir.path().join("App.xcodeproj")).unwrap();
    assert!(matches!(kind(&dir), ProjectKind::Unsupported { .. }));

    fs::write(dir.path().join("Dockerfile"), "").unwrap();
    let found = detect(dir.path()).unwrap();
    assert!(matches!(found.kind, ProjectKind::Dockerfile { .. }));
    assert!(found.notes.iter().any(|n| n.contains("App.xcodeproj")));
}

#[test]
fn a_file_named_like_an_xcode_project_is_not_a_marker() {
    let dir = project(&["App.xcodeproj"]);
    assert_eq!(kind(&dir), ProjectKind::Unknown);
}

#[test]
fn package_swift_with_apple_only_platform_is_unsupported() {
    let dir = project(&[]);
    fs::write(
        dir.path().join("Package.swift"),
        "let package = Package(name: \"a\", platforms: [.iOS(.v17)])",
    )
    .unwrap();
    assert!(matches!(kind(&dir), ProjectKind::Unsupported { .. }));
}

#[test]
fn server_side_package_swift_is_a_manifest() {
    let dir = project(&[]);
    fs::write(
        dir.path().join("Package.swift"),
        "let package = Package(name: \"a\", platforms: [.macOS(.v13)])",
    )
    .unwrap();
    assert!(matches!(kind(&dir), ProjectKind::Manifest { .. }));
}

#[test]
fn oversized_package_swift_is_noted_and_not_read() {
    let dir = project(&[]);
    fs::write(dir.path().join("Package.swift"), ".iOS(".repeat(100_000)).unwrap();
    let found = detect(dir.path()).unwrap();
    assert!(matches!(found.kind, ProjectKind::Manifest { .. }));
    assert!(found.notes.iter().any(|n| n.contains("Package.swift")));
}

#[cfg(unix)]
mod links {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn compose_link_out_of_project_is_skipped() {
        let outside = project(&["evil.yml"]);
        let dir = project(&["package.json"]);
        symlink(
            outside.path().join("evil.yml"),
            dir.path().join("compose.yml"),
        )
        .unwrap();
        let found = detect(dir.path()).unwrap();
        assert!(matches!(found.kind, ProjectKind::Manifest { .. }));
        assert!(found.notes.iter().any(|n| n.contains("compose.yml")));
    }

    #[test]
    fn compose_link_inside_project_is_used() {
        let dir = project(&["deploy/stack.yml"]);
        symlink(
            dir.path().join("deploy/stack.yml"),
            dir.path().join("compose.yml"),
        )
        .unwrap();
        assert!(matches!(kind(&dir), ProjectKind::Compose { .. }));
    }

    #[test]
    fn devcontainer_directory_link_out_is_skipped() {
        let outside = project(&["devcontainer.json"]);
        let dir = project(&[]);
        symlink(outside.path(), dir.path().join(".devcontainer")).unwrap();
        assert_eq!(kind(&dir), ProjectKind::Unknown);
    }

    #[test]
    fn broken_and_looping_links_are_skipped() {
        let dir = project(&[]);
        symlink("/nonexistent/target", dir.path().join("Dockerfile")).unwrap();
        symlink("compose.yml", dir.path().join("compose.yml")).unwrap();
        let found = detect(dir.path()).unwrap();
        assert_eq!(found.kind, ProjectKind::Unknown);
        assert_eq!(found.notes.len(), 2);
    }

    #[test]
    fn linked_xcode_bundle_is_ignored() {
        let outside = TempDir::new().unwrap();
        let dir = project(&[]);
        symlink(outside.path(), dir.path().join("App.xcodeproj")).unwrap();
        assert_eq!(kind(&dir), ProjectKind::Unknown);
    }

    #[test]
    fn hostile_file_names_in_notes_are_safe_to_print() {
        let dir = project(&[]);
        fs::create_dir(dir.path().join("a\u{1b}[31m.xcodeproj")).unwrap();
        fs::write(dir.path().join("Dockerfile"), "").unwrap();
        let found = detect(dir.path()).unwrap();
        assert!(found.notes.iter().all(|n| !n.contains('\u{1b}')));
        assert!(!found.notes.is_empty());
    }
}

#[test]
fn a_directory_named_dockerfile_is_skipped() {
    let dir = project(&["Dockerfile/keep", "go.mod"]);
    assert!(matches!(kind(&dir), ProjectKind::Manifest { .. }));
}

#[test]
fn env_files_do_not_affect_detection() {
    let dir = project(&[".env", ".env.local"]);
    assert_eq!(kind(&dir), ProjectKind::Unknown);
}
