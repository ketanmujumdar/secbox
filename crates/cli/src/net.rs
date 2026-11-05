//! `net check` explains one decision: nothing is sent, and no name is looked up.
//! `net blocked` is in [`crate::blocked`].

use std::io::Read;
use std::path::PathBuf;

use clap::{Args, Subcommand};
use egress::{Decision, Policy, Request, MAX_POLICY_FILE_BYTES};

use crate::ui::{cell, esc, esc_path, not_protecting, paint, Tone};
use crate::{exit, say, tell, Deps};

const LABEL: usize = 10;

#[derive(Args)]
pub struct NetArgs {
    #[command(subcommand)]
    command: NetCommand,
}

#[derive(Subcommand)]
enum NetCommand {
    /// Explain whether a policy file allows one request
    Check(CheckArgs),
    /// Show blocked hostnames recorded for a project. Raw IP, metadata and UDP attempts are not recorded
    Blocked(crate::blocked::BlockedArgsCli),
}

#[derive(Args)]
struct CheckArgs {
    /// The policy YAML file
    policy: PathBuf,
    /// HTTP method, uppercase, such as GET
    method: String,
    /// Full URL, such as https://registry.npmjs.org/left-pad
    url: String,
}

pub fn run(args: &NetArgs, deps: &mut Deps) -> i32 {
    match &args.command {
        NetCommand::Check(args) => check(args, deps),
        NetCommand::Blocked(args) => crate::blocked::run(args, deps),
    }
}

fn check(args: &CheckArgs, deps: &mut Deps) -> i32 {
    let policy = match load_policy(args, deps) {
        Ok(policy) => policy,
        Err(reason) => {
            tell!(deps, "net check: {reason}");
            return exit::FAILED;
        }
    };
    let target = match parse_url(&args.url) {
        Ok(target) => target,
        Err(reason) => {
            tell!(deps, "net check: {reason}");
            return exit::USAGE;
        }
    };
    let request = Request {
        host: target.host.clone(),
        port: target.port,
        method: args.method.clone(),
        path: target.path.clone(),
        scheme: target.scheme.clone(),
    };
    let decision = policy.decide(&request);

    field(
        deps,
        "Policy",
        &format!("{}, {} rules", esc_path(&args.policy), policy.rules().len()),
    );
    field(
        deps,
        "Request",
        &format!("{} {}", esc(&args.method), esc(&target.shown())),
    );
    let code = match &decision {
        Decision::Allow { rule, host } => {
            let word = paint(deps.color, Tone::Good, "allow");
            field(deps, "Decision", &word);
            field(deps, "Rule", &esc(rule));
            field(deps, "Host", &esc(host));
            exit::OK
        }
        Decision::Deny { reason } => {
            let word = paint(deps.color, Tone::Bad, "deny");
            field(deps, "Decision", &word);
            field(deps, "Reason", &esc(&reason.to_string()));
            exit::DENIED
        }
    };
    say!(deps);
    say!(
        deps,
        "This only explains the policy. Nothing was sent, and no name was looked up."
    );
    say!(
        deps,
        "A real proxy would also check the address a name resolves to."
    );
    say!(deps, "{}", paint(deps.color, Tone::Warn, &not_protecting()));
    code
}

fn field(deps: &mut Deps, label: &str, value: &str) {
    say!(
        deps,
        "{}{value}",
        cell(deps.color, Tone::Strong, label, LABEL)
    );
}

fn load_policy(args: &CheckArgs, deps: &Deps) -> Result<Policy, String> {
    let path = if args.policy.is_absolute() {
        args.policy.clone()
    } else {
        deps.cwd.join(&args.policy)
    };
    let shown = esc_path(&args.policy);
    let meta =
        std::fs::metadata(&path).map_err(|e| format!("cannot open {shown}: {}", e.kind()))?;
    if !meta.is_file() {
        return Err(format!("{shown} is not a regular file"));
    }
    let file =
        std::fs::File::open(&path).map_err(|e| format!("cannot open {shown}: {}", e.kind()))?;
    let mut text = String::new();
    file.take(MAX_POLICY_FILE_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("cannot read {shown}: {}", e.kind()))?;
    Policy::from_yaml(&text).map_err(|e| format!("{shown}: {}", esc(&e.to_string())))
}

struct Target {
    scheme: String,
    host: String,
    /// The host part to print. Any `user:password@` is left out.
    shown_host: String,
    port: u16,
    path: String,
}

impl Target {
    /// Leaves out the query, which can hold credentials.
    fn shown(&self) -> String {
        let (path, query) = match self.path.split_once('?') {
            Some((path, _)) => (path, "?<query hidden>"),
            None => (self.path.as_str(), ""),
        };
        format!(
            "{}://{}:{}{path}{query}",
            self.scheme, self.shown_host, self.port
        )
    }
}

/// Splits a URL without normalizing the host, since the policy engine does
/// that. Userinfo stays in the host, so that the engine refuses it.
fn parse_url(url: &str) -> Result<Target, &'static str> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or("the URL needs a scheme, such as https://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    if authority.is_empty() {
        return Err("the URL has no host");
    }
    let tail = tail.split('#').next().unwrap_or("");
    let path = match tail.chars().next() {
        None => "/".to_string(),
        Some('/') => tail.to_string(),
        Some(_) => format!("/{tail}"),
    };
    let default_port = match scheme.to_ascii_lowercase().as_str() {
        "http" => 80,
        _ => 443,
    };
    let (host, port) = if authority.contains('@') {
        (authority, default_port)
    } else if let Some(inner) = authority.strip_prefix('[') {
        let close = inner.find(']').ok_or("the URL has an unclosed [")?;
        let after = &inner[close + 1..];
        let host = &authority[..close + 2];
        match after.strip_prefix(':') {
            Some(port) => (host, parse_port(port)?),
            None if after.is_empty() => (host, default_port),
            None => return Err("the URL has text after the closing ]"),
        }
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') => (host, parse_port(port)?),
            Some(_) => (authority, default_port),
            None => (authority, default_port),
        }
    };
    let shown_host = match authority.rsplit_once('@') {
        Some((_, after)) => format!("<userinfo hidden>@{after}"),
        None => host.to_string(),
    };
    Ok(Target {
        scheme: scheme.to_string(),
        host: host.to_string(),
        shown_host,
        port,
        path,
    })
}

fn parse_port(text: &str) -> Result<u16, &'static str> {
    text.parse().map_err(|_| "the URL has an invalid port")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_urls() {
        let t = parse_url("https://Example.com:8443/a/b?x=1#frag").unwrap();
        assert_eq!(
            (t.scheme.as_str(), t.host.as_str(), t.port, t.path.as_str()),
            ("https", "Example.com", 8443, "/a/b?x=1")
        );
        let t = parse_url("http://example.com").unwrap();
        assert_eq!((t.port, t.path.as_str()), (80, "/"));
        let t = parse_url("https://[2606:4700::1]:444/x").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("[2606:4700::1]", 444));
        let t = parse_url("https://example.com?x=1").unwrap();
        assert_eq!(t.path, "/?x=1");
    }

    #[test]
    fn hides_userinfo_and_query_when_shown() {
        let t = parse_url("https://user:hunter2@example.com/p?token=abc").unwrap();
        assert!(t.host.contains('@'));
        let shown = t.shown();
        assert!(!shown.contains("hunter2"));
        assert!(!shown.contains("abc"));
    }

    #[test]
    fn refuses_nonsense() {
        for bad in [
            "example.com/x",
            "https://",
            "https://a:99999/",
            "https://[::1",
        ] {
            assert!(parse_url(bad).is_err(), "{bad}");
        }
    }
}
