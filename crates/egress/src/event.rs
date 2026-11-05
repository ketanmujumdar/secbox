use std::fmt;

use serde::Serialize;

use crate::policy::{DenyReason, Request};

const MAX_HOST: usize = 255;
const MAX_METHOD: usize = 32;
const MAX_PATH: usize = 1024;
const MAX_SCHEME: usize = 16;
const MAX_REASON: usize = 256;

/// A log record for a denied request.
///
/// Everything here came from a possibly compromised sandbox. Query strings,
/// fragments and userinfo are dropped because they can hold credentials, and
/// everything except visible ASCII is escaped, so a request cannot forge log
/// lines or send terminal control sequences to whoever reads them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockedEvent {
    pub host: String,
    pub port: u16,
    pub method: String,
    pub path: String,
    pub scheme: String,
    pub reason: String,
}

impl BlockedEvent {
    pub fn new(request: &Request, reason: &DenyReason) -> Self {
        let (host, method, path, scheme) = scrub_for_debug(request);
        Self {
            host,
            port: request.port,
            method,
            path,
            scheme,
            reason: escape(&reason.to_string(), MAX_REASON),
        }
    }
}

impl fmt::Display for BlockedEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "blocked {} {}://{}:{}{} ({})",
            self.method, self.scheme, self.host, self.port, self.path, self.reason
        )
    }
}

/// Returns host, method, path and scheme, made safe to display.
pub(crate) fn scrub_for_debug(request: &Request) -> (String, String, String, String) {
    (
        escape(scrub_host(&request.host), MAX_HOST),
        escape(&request.method, MAX_METHOD),
        scrub_path(&request.path),
        escape(&request.scheme, MAX_SCHEME),
    )
}

/// Keeps only what follows any userinfo, and stops at any path or query.
fn scrub_host(host: &str) -> &str {
    let after_userinfo = host.rsplit('@').next().unwrap_or(host);
    let end = after_userinfo
        .find(['/', '?', '#'])
        .unwrap_or(after_userinfo.len());
    &after_userinfo[..end]
}

fn scrub_path(path: &str) -> String {
    // A target that is not origin-form may be a whole URL with userinfo in it.
    if !path.starts_with('/') {
        return "<not a path>".to_owned();
    }
    escape(crate::http::path_only(path), MAX_PATH)
}

fn escape(text: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max_chars));
    for (index, c) in text.chars().enumerate() {
        if index == max_chars {
            out.push_str("...");
            break;
        }
        match c {
            '\\' => out.push_str("\\\\"),
            ' '..='~' => out.push(c),
            _ => out.extend(c.escape_unicode()),
        }
    }
    out
}
