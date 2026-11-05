//! Which build hosts a project gets, from a fixed menu.
//!
//! The engine fixes a sandbox's network rules when it is made, so every build
//! host stays reachable while the project runs, from every service, including
//! one that holds a secret. Some of these hosts take uploads from anyone. The
//! fewer of them a sandbox gets, the fewer places a stolen value can go.
//!
//! The project's files choose groups from the menu below and nothing else: a
//! hostile repo can switch on a group that a real project might need, but can
//! never name a host of its own. Container image hosts are always on, since
//! every Compose project pulls or builds an image.
//!
//! The walk is bounded and follows no links. A project the detection misses
//! fails its build with a refused name, which `net blocked` shows.

use std::path::Path;

/// One group of hosts and why a project would need it.
pub struct Group {
    pub name: &'static str,
    pub hosts: &'static [&'static str],
}

/// Pulling images from Docker Hub. Every project needs these.
pub const IMAGES: Group = Group {
    name: "container images",
    hosts: &[
        "registry-1.docker.io",
        "auth.docker.io",
        "production.cloudflare.docker.com",
        "production.cloudfront.docker.com",
        "docker-images-prod.6aa30f8b08e16409b46e0173d6de2f56.r2.cloudflarestorage.com",
    ],
};

pub const ALPINE: Group = Group {
    name: "Alpine packages",
    hosts: &["dl-cdn.alpinelinux.org"],
};

pub const NPM: Group = Group {
    name: "npm packages",
    hosts: &["registry.npmjs.org"],
};

/// Go modules. The engine spike measured `storage.googleapis.com` alongside
/// these, and it accepts uploads from anyone, so it is kept to Go projects.
pub const GO: Group = Group {
    name: "Go modules",
    hosts: &["proxy.golang.org", "sum.golang.org", "storage.googleapis.com"],
};

/// Web fonts fetched at build time, as Next.js does for Google fonts.
pub const FONTS: Group = Group {
    name: "web fonts",
    hosts: &["fonts.googleapis.com", "fonts.gstatic.com"],
};

pub const ALL: [&Group; 5] = [&IMAGES, &ALPINE, &NPM, &GO, &FONTS];

const MAX_DEPTH: usize = 8;
const MAX_ENTRIES: usize = 20_000;
const MAX_READ: u64 = 256 * 1024;
/// Folders that are output or downloads, not the project's own choices.
const SKIP: [&str; 6] = ["node_modules", ".git", "vendor", "target", "dist", ".next"];

#[derive(Default)]
struct Found {
    alpine: bool,
    npm: bool,
    go: bool,
    fonts: bool,
}

/// The groups this project's files point to, with the images group first.
pub fn groups_for(root: &Path) -> Vec<&'static Group> {
    let mut found = Found::default();
    let mut seen = 0;
    walk(root, 0, &mut seen, &mut found);
    let mut groups = vec![&IMAGES];
    for (on, group) in [
        (found.alpine, &ALPINE),
        (found.npm, &NPM),
        (found.go, &GO),
        (found.fonts, &FONTS),
    ] {
        if on {
            groups.push(group);
        }
    }
    groups
}

pub fn hosts_for(root: &Path) -> Vec<String> {
    groups_for(root)
        .iter()
        .flat_map(|g| g.hosts.iter().map(|h| (*h).to_owned()))
        .collect()
}

fn walk(dir: &Path, depth: usize, seen: &mut usize, found: &mut Found) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        *seen += 1;
        if *seen > MAX_ENTRIES {
            return;
        }
        // Never through a link: one could point anywhere on the host.
        let Ok(kind) = entry.file_type() else { continue };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if kind.is_dir() {
            if !SKIP.contains(&name.as_ref()) {
                walk(&entry.path(), depth + 1, seen, found);
            }
            continue;
        }
        if !kind.is_file() {
            continue;
        }
        look_at(&entry.path(), &name, found);
    }
}

fn look_at(path: &Path, name: &str, found: &mut Found) {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "package.json" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" => found.npm = true,
        "go.mod" | "go.sum" => found.go = true,
        _ => {}
    }
    let is_dockerfile = lower.starts_with("dockerfile")
        || lower.ends_with(".dockerfile")
        || lower.starts_with("containerfile");
    let is_source = [".js", ".jsx", ".ts", ".tsx", ".mjs", ".css", ".scss"]
        .iter()
        .any(|ext| lower.ends_with(ext));
    if !is_dockerfile && !(is_source && !found.fonts) {
        return;
    }
    let Some(text) = read_small(path) else { return };
    if is_dockerfile {
        let words = text.to_ascii_lowercase();
        if words.contains("apk ") || words.contains("alpine") {
            found.alpine = true;
        }
        if ["npm ", "npx ", "yarn", "pnpm", "corepack"].iter().any(|w| words.contains(w)) {
            found.npm = true;
        }
        if ["go build", "go mod", "go get", "go install", "golang"].iter().any(|w| words.contains(w)) {
            found.go = true;
        }
    }
    if is_source && (text.contains("next/font/google") || text.contains("fonts.googleapis.com")) {
        found.fonts = true;
    }
}

fn read_small(path: &Path) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut text = String::new();
    file.take(MAX_READ).read_to_string(&mut text).ok()?;
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, body) in files {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        dir
    }

    fn names(root: &Path) -> Vec<&'static str> {
        groups_for(root).iter().map(|g| g.name).collect()
    }

    #[test]
    fn a_plain_image_project_gets_only_the_image_hosts() {
        let p = project(&[("compose.yaml", "services: {}\n"), ("web/Dockerfile", "FROM scratch\n")]);
        assert_eq!(names(p.path()), vec!["container images"]);
        let hosts = hosts_for(p.path());
        assert!(!hosts.iter().any(|h| h == "storage.googleapis.com"));
        assert!(!hosts.iter().any(|h| h == "registry.npmjs.org"));
    }

    #[test]
    fn a_node_project_on_alpine_gets_npm_and_alpine_but_not_go_storage() {
        let p = project(&[
            ("web/package.json", "{}"),
            ("web/Dockerfile", "FROM node:22-alpine\nRUN apk add git\nRUN npm ci\n"),
        ]);
        assert_eq!(names(p.path()), vec!["container images", "Alpine packages", "npm packages"]);
        assert!(!hosts_for(p.path()).iter().any(|h| h == "storage.googleapis.com"));
    }

    #[test]
    fn go_and_fonts_are_found_from_their_files() {
        let p = project(&[
            ("svc/go.mod", "module x\n"),
            ("web/app/layout.tsx", "import { Inter } from 'next/font/google'\n"),
        ]);
        assert_eq!(names(p.path()), vec!["container images", "Go modules", "web fonts"]);
    }

    #[test]
    fn downloaded_dependencies_choose_nothing() {
        let p = project(&[("node_modules/x/go.mod", "module x\n")]);
        assert_eq!(names(p.path()), vec!["container images"]);
    }

    #[test]
    fn a_link_out_of_the_project_is_not_followed() {
        let outside = project(&[("go.mod", "module x\n")]);
        let p = project(&[("compose.yaml", "services: {}\n")]);
        std::os::unix::fs::symlink(outside.path(), p.path().join("linked")).unwrap();
        assert_eq!(names(p.path()), vec!["container images"]);
    }

    #[test]
    fn every_host_is_a_plain_name() {
        for group in ALL {
            for host in group.hosts {
                assert!(!host.contains('*') && !host.contains('/'), "{host}");
            }
        }
    }
}
