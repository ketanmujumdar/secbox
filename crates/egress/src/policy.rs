use std::fmt;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::event::scrub_for_debug;
use crate::host::{classify_ip, is_reserved_name, normalize_host, Host, HostError, IpClass};
use crate::http::{is_valid_method, is_valid_path, is_valid_path_prefix, path_has_prefix};

/// The largest accepted policy file.
pub const MAX_POLICY_FILE_BYTES: usize = 1024 * 1024;

const POLICY_VERSION: u32 = 1;

/// A request as the proxy saw it. All fields are untrusted.
#[derive(Clone, PartialEq, Eq)]
pub struct Request {
    pub host: String,
    pub port: u16,
    pub method: String,
    /// The request target, starting with `/`. It may include a query.
    pub path: String,
    pub scheme: String,
}

impl fmt::Debug for Request {
    /// Query strings can hold credentials, so `Debug` leaves them out.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (host, method, path, scheme) = scrub_for_debug(self);
        f.debug_struct("Request")
            .field("host", &host)
            .field("port", &self.port)
            .field("method", &method)
            .field("path", &path)
            .field("scheme", &scheme)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scheme {
    Http,
    Https,
}

impl Scheme {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        if text.eq_ignore_ascii_case("https") {
            Some(Scheme::Https)
        } else if text.eq_ignore_ascii_case("http") {
            Some(Scheme::Http)
        } else {
            None
        }
    }
}

impl fmt::Display for Scheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Scheme::Http => "http",
            Scheme::Https => "https",
        })
    }
}

/// The outcome of a check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow {
        /// The label of the rule that allowed it.
        rule: String,
        /// The normalized host. The proxy should use this, not the raw input.
        host: String,
    },
    Deny {
        reason: DenyReason,
    },
}

impl Decision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Decision::Allow { .. })
    }

    pub fn rule(&self) -> Option<&str> {
        match self {
            Decision::Allow { rule, .. } => Some(rule),
            Decision::Deny { .. } => None,
        }
    }

    pub fn deny_reason(&self) -> Option<&DenyReason> {
        match self {
            Decision::Allow { .. } => None,
            Decision::Deny { reason } => Some(reason),
        }
    }
}

/// Why something was denied. Holds rule labels and fixed categories only.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    #[error("no rule matches this host")]
    NoMatchingRule,
    #[error("invalid host: {0}")]
    InvalidHost(HostError),
    #[error("scheme must be http or https")]
    InvalidScheme,
    #[error("method must be an uppercase token")]
    InvalidMethod,
    #[error("port 0 is not valid")]
    InvalidPort,
    #[error("request target must be an origin-form ASCII path")]
    InvalidPath,
    #[error("this name always refers to the local machine or a metadata service")]
    ReservedName,
    #[error("address class {0:?} cannot be reached by this request")]
    BlockedAddress(IpClass),
    #[error("IP literals need a rule naming that exact IP")]
    IpLiteralNotAllowed,
    #[error("rule {rule} does not allow this scheme")]
    SchemeNotAllowed { rule: String },
    #[error("rule {rule} does not allow this port")]
    PortNotAllowed { rule: String },
    #[error("rule {rule} does not allow this method")]
    MethodNotAllowed { rule: String },
    #[error("rule {rule} does not allow this path")]
    PathNotAllowed { rule: String },
    #[error("resolved address class {0:?} is not allowed for a named host")]
    ResolvedAddressBlocked(IpClass),
    #[error("resolved address does not match the IP in the request")]
    ResolvedAddressMismatch,
}

/// A problem in a rule or policy file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("invalid host in rule: {0}")]
    InvalidHost(HostError),
    #[error("rule name must be 1 to 128 visible ASCII characters or spaces")]
    InvalidRuleName,
    #[error("wildcards must be a leading `*.` above at least two labels, such as `*.example.com`")]
    InvalidWildcard,
    #[error("this name always refers to the local machine or a metadata service")]
    ReservedName,
    #[error("no rule may name address class {0:?}")]
    BlockedAddress(IpClass),
    #[error("a rule must list at least one port")]
    NoPorts,
    #[error("port 0 is not valid")]
    InvalidPort,
    #[error("a rule must list at least one method")]
    NoMethods,
    #[error("rule methods must be uppercase tokens")]
    InvalidMethod,
    #[error("CONNECT and TRACE cannot be allowed by a rule")]
    ForbiddenMethod,
    #[error("a rule must list at least one scheme")]
    NoSchemes,
    #[error("path prefix must be a plain absolute path with no dot segments or encoding tricks")]
    InvalidPathPrefix,
    #[error("policy file is larger than the limit of {limit} bytes")]
    TooLarge { limit: usize },
    #[error("unsupported policy version {0}")]
    UnsupportedVersion(u32),
    #[error("could not parse policy file: {0}")]
    Parse(String),
}

/// What a rule's host names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HostPattern {
    Exact(String),
    /// `*.base`. Matches names under `base`, and not `base` itself.
    Wildcard(String),
    Ip(IpAddr),
}

impl HostPattern {
    fn parse(text: &str) -> Result<Self, PolicyError> {
        if let Some(base) = text.strip_prefix("*.") {
            let name = match normalize_host(base) {
                Ok(Host::Name(name)) => name,
                _ => return Err(PolicyError::InvalidWildcard),
            };
            // `*.com` would cover a whole top-level domain.
            if name.split('.').count() < 2 {
                return Err(PolicyError::InvalidWildcard);
            }
            if is_reserved_name(&name) {
                return Err(PolicyError::ReservedName);
            }
            return Ok(Self::Wildcard(name));
        }
        if text.contains('*') {
            return Err(PolicyError::InvalidWildcard);
        }
        match normalize_host(text).map_err(PolicyError::InvalidHost)? {
            Host::Name(name) if is_reserved_name(&name) => Err(PolicyError::ReservedName),
            Host::Name(name) => Ok(Self::Exact(name)),
            Host::Ip(ip) => {
                let class = classify_ip(ip);
                if class.is_always_denied() {
                    Err(PolicyError::BlockedAddress(class))
                } else {
                    Ok(Self::Ip(ip))
                }
            }
        }
    }

    fn matches(&self, host: &Host) -> bool {
        match (self, host) {
            (Self::Exact(pattern), Host::Name(name)) => pattern == name,
            (Self::Wildcard(base), Host::Name(name)) => is_strict_subdomain(name, base),
            (Self::Ip(pattern), Host::Ip(ip)) => pattern == ip,
            _ => false,
        }
    }

    /// True if everything `other` can match, this can match too.
    fn covers(&self, other: &Self) -> bool {
        match (self, other) {
            (a, b) if a == b => true,
            (Self::Wildcard(base), Self::Exact(name)) => is_strict_subdomain(name, base),
            (Self::Wildcard(base), Self::Wildcard(inner)) => is_strict_subdomain(inner, base),
            _ => false,
        }
    }
}

/// Checks on a dot boundary, so `evil-npmjs.org` is not under `npmjs.org`.
fn is_strict_subdomain(name: &str, base: &str) -> bool {
    name.strip_suffix(base)
        .and_then(|front| front.strip_suffix('.'))
        .is_some_and(|front| !front.is_empty())
}

impl fmt::Display for HostPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exact(name) => f.write_str(name),
            Self::Wildcard(base) => write!(f, "*.{base}"),
            Self::Ip(ip) => Host::Ip(*ip).fmt(f),
        }
    }
}

/// The unvalidated form of a rule, as it appears in a policy file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSpec {
    /// A label for logs. Defaults to the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// An exact name, `*.example.com`, or an exact IP.
    pub host: String,
    pub ports: Vec<u16>,
    pub methods: Vec<String>,
    /// Defaults to `https` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schemes: Option<Vec<Scheme>>,
    /// If set, only paths under this prefix are allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<String>,
}

/// One validated allow rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    label: Option<String>,
    host: HostPattern,
    ports: Vec<u16>,
    methods: Vec<String>,
    schemes: Vec<Scheme>,
    path_prefix: Option<String>,
}

impl Rule {
    pub fn new(spec: RuleSpec) -> Result<Self, PolicyError> {
        let host = HostPattern::parse(&spec.host)?;
        // Labels reach logs, and a policy file can come from an untrusted repo.
        if let Some(name) = &spec.name {
            let printable = name.bytes().all(|b| (b' '..=b'~').contains(&b));
            if name.is_empty() || name.len() > 128 || !printable {
                return Err(PolicyError::InvalidRuleName);
            }
        }
        if spec.ports.is_empty() {
            return Err(PolicyError::NoPorts);
        }
        if spec.ports.contains(&0) {
            return Err(PolicyError::InvalidPort);
        }
        if spec.methods.is_empty() {
            return Err(PolicyError::NoMethods);
        }
        for method in &spec.methods {
            if !is_valid_method(method) {
                return Err(PolicyError::InvalidMethod);
            }
            // CONNECT opens a tunnel that later checks cannot see into, and
            // TRACE echoes request headers back, including injected keys.
            if method == "CONNECT" || method == "TRACE" {
                return Err(PolicyError::ForbiddenMethod);
            }
        }
        let schemes = spec.schemes.unwrap_or_else(|| vec![Scheme::Https]);
        if schemes.is_empty() {
            return Err(PolicyError::NoSchemes);
        }
        if let Some(prefix) = &spec.path_prefix {
            if !is_valid_path_prefix(prefix) {
                return Err(PolicyError::InvalidPathPrefix);
            }
        }
        Ok(Self {
            label: spec.name,
            host,
            ports: spec.ports,
            methods: spec.methods,
            schemes,
            path_prefix: spec.path_prefix,
        })
    }

    /// The name shown in decisions and logs.
    pub fn label(&self) -> String {
        match &self.label {
            Some(label) => label.clone(),
            None => self.host.to_string(),
        }
    }

    pub fn to_spec(&self) -> RuleSpec {
        RuleSpec {
            name: self.label.clone(),
            host: self.host.to_string(),
            ports: self.ports.clone(),
            methods: self.methods.clone(),
            schemes: Some(self.schemes.clone()),
            path_prefix: self.path_prefix.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    version: u32,
    #[serde(default)]
    rules: Vec<RuleSpec>,
}

/// A set of allow rules. Anything no rule allows is denied.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    rules: Vec<Rule>,
}

impl Policy {
    pub fn new(rules: Vec<Rule>) -> Self {
        Self { rules }
    }

    /// A policy with no rules, which denies everything.
    pub fn deny_all() -> Self {
        Self::default()
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Parses a policy file. Unknown keys, at any level, are errors.
    pub fn from_yaml(text: &str) -> Result<Self, PolicyError> {
        if text.len() > MAX_POLICY_FILE_BYTES {
            return Err(PolicyError::TooLarge {
                limit: MAX_POLICY_FILE_BYTES,
            });
        }
        let file: PolicyFile =
            serde_norway::from_str(text).map_err(|e| PolicyError::Parse(e.to_string()))?;
        if file.version != POLICY_VERSION {
            return Err(PolicyError::UnsupportedVersion(file.version));
        }
        let rules = file
            .rules
            .into_iter()
            .map(Rule::new)
            .collect::<Result<_, _>>()?;
        Ok(Self { rules })
    }

    pub fn to_yaml(&self) -> Result<String, PolicyError> {
        let file = PolicyFile {
            version: POLICY_VERSION,
            rules: self.rules.iter().map(Rule::to_spec).collect(),
        };
        serde_norway::to_string(&file).map_err(|e| PolicyError::Parse(e.to_string()))
    }

    /// Decides one request. Anything not explicitly allowed is denied.
    pub fn decide(&self, request: &Request) -> Decision {
        match self.evaluate(request) {
            Ok((rule, host)) => Decision::Allow { rule, host },
            Err(reason) => Decision::Deny { reason },
        }
    }

    fn evaluate(&self, request: &Request) -> Result<(String, String), DenyReason> {
        let scheme = Scheme::parse(&request.scheme).ok_or(DenyReason::InvalidScheme)?;
        if !is_valid_method(&request.method) {
            return Err(DenyReason::InvalidMethod);
        }
        if request.port == 0 {
            return Err(DenyReason::InvalidPort);
        }
        if !is_valid_path(&request.path) {
            return Err(DenyReason::InvalidPath);
        }
        let host = normalize_host(&request.host).map_err(DenyReason::InvalidHost)?;
        check_host_class(&host)?;

        // Report the failure from the rule that got furthest, for a useful log.
        let mut best: Option<(u8, DenyReason)> = None;
        for rule in self.rules.iter().filter(|r| r.host.matches(&host)) {
            let failure = if !rule.schemes.contains(&scheme) {
                (0, DenyReason::SchemeNotAllowed { rule: rule.label() })
            } else if !rule.ports.contains(&request.port) {
                (1, DenyReason::PortNotAllowed { rule: rule.label() })
            } else if !rule.methods.contains(&request.method) {
                (2, DenyReason::MethodNotAllowed { rule: rule.label() })
            } else if rule
                .path_prefix
                .as_deref()
                .is_some_and(|prefix| !path_has_prefix(&request.path, prefix))
            {
                (3, DenyReason::PathNotAllowed { rule: rule.label() })
            } else {
                return Ok((rule.label(), host.to_string()));
            };
            if best.as_ref().is_none_or(|(rank, _)| failure.0 > *rank) {
                best = Some(failure);
            }
        }
        Err(match (best, &host) {
            (Some((_, reason)), _) => reason,
            (None, Host::Ip(ip)) => match classify_ip(*ip) {
                IpClass::Public => DenyReason::IpLiteralNotAllowed,
                class => DenyReason::BlockedAddress(class),
            },
            (None, Host::Name(_)) => DenyReason::NoMatchingRule,
        })
    }

    /// Checks an address that `host` resolved to, so DNS rebinding cannot turn
    /// an allowed name into a private target.
    ///
    /// A name may resolve only to public addresses, even with an exact-IP rule
    /// for the private one. An IP literal must equal `ip`, and must have an
    /// exact-IP rule.
    pub fn check_resolved(&self, host: &str, ip: IpAddr) -> Decision {
        match self.evaluate_resolved(host, ip) {
            Ok((rule, host)) => Decision::Allow { rule, host },
            Err(reason) => Decision::Deny { reason },
        }
    }

    fn evaluate_resolved(&self, host: &str, ip: IpAddr) -> Result<(String, String), DenyReason> {
        let host = normalize_host(host).map_err(DenyReason::InvalidHost)?;
        check_host_class(&host)?;
        let class = classify_ip(ip);
        if class.is_always_denied() {
            return Err(DenyReason::ResolvedAddressBlocked(class));
        }
        match &host {
            Host::Ip(literal) if *literal != crate::host::canonical_ip(ip) => {
                return Err(DenyReason::ResolvedAddressMismatch);
            }
            Host::Name(_) if class != IpClass::Public => {
                return Err(DenyReason::ResolvedAddressBlocked(class));
            }
            _ => {}
        }
        let rule = self
            .rules
            .iter()
            .find(|r| r.host.matches(&host))
            .ok_or(match host {
                Host::Ip(_) => DenyReason::IpLiteralNotAllowed,
                Host::Name(_) => DenyReason::NoMatchingRule,
            })?;
        Ok((rule.label(), host.to_string()))
    }
}

/// Denials that no rule can change.
fn check_host_class(host: &Host) -> Result<(), DenyReason> {
    match host {
        Host::Name(name) if is_reserved_name(name) => Err(DenyReason::ReservedName),
        Host::Ip(ip) if classify_ip(*ip).is_always_denied() => {
            Err(DenyReason::BlockedAddress(classify_ip(*ip)))
        }
        _ => Ok(()),
    }
}

/// How a policy change affects what is reachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// Nothing is allowed that was not before, and nothing is lost.
    Unchanged,
    /// Some access is removed, and none is added.
    Tightening,
    /// Some access is added. This includes changes that also remove access.
    Loosening,
}

/// The result of comparing two policies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PolicyChange {
    pub kind: ChangeKind,
    /// Grants in the new policy that the old one did not cover.
    pub added: Vec<String>,
    /// Grants in the old policy that the new one does not cover.
    pub removed: Vec<String>,
}

impl PolicyChange {
    /// True if a person must approve this change.
    pub fn requires_approval(&self) -> bool {
        self.kind == ChangeKind::Loosening
    }
}

/// Compares two policies by what they allow, not by how they are written, so
/// reordering or relabelling rules is `Unchanged`.
///
/// This errs toward `Loosening`. A grant counts as covered only if one old rule
/// covers it on its own, so splitting a path prefix across rules can be
/// reported as loosening when it is not. It never reports the reverse.
pub fn classify_change(old: &Policy, new: &Policy) -> PolicyChange {
    let added = uncovered_grants(new, old);
    let removed = uncovered_grants(old, new);
    let kind = if !added.is_empty() {
        ChangeKind::Loosening
    } else if !removed.is_empty() {
        ChangeKind::Tightening
    } else {
        ChangeKind::Unchanged
    };
    PolicyChange {
        kind,
        added,
        removed,
    }
}

/// Lists grants in `policy` that no single rule in `other` covers.
fn uncovered_grants(policy: &Policy, other: &Policy) -> Vec<String> {
    let mut grants = Vec::new();
    for rule in &policy.rules {
        for scheme in &rule.schemes {
            for port in &rule.ports {
                for method in &rule.methods {
                    let covered = other.rules.iter().any(|o| {
                        o.schemes.contains(scheme)
                            && o.ports.contains(port)
                            && o.methods.contains(method)
                            && o.host.covers(&rule.host)
                            && prefix_covers(o.path_prefix.as_deref(), rule.path_prefix.as_deref())
                    });
                    if !covered {
                        grants.push(format!(
                            "{method} {scheme}://{}:{port}{}",
                            rule.host,
                            rule.path_prefix.as_deref().unwrap_or("")
                        ));
                    }
                }
            }
        }
    }
    grants
}

fn prefix_covers(wider: Option<&str>, narrower: Option<&str>) -> bool {
    match (wider, narrower) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(wider), Some(narrower)) => path_has_prefix(narrower, wider),
    }
}

impl Rule {
    pub(crate) fn from_parts(
        label: &str,
        host: HostPattern,
        methods: &[&str],
        path_prefix: Option<String>,
    ) -> Self {
        Self {
            label: Some(label.to_owned()),
            host,
            ports: vec![443],
            methods: methods.iter().map(|m| (*m).to_owned()).collect(),
            schemes: vec![Scheme::Https],
            path_prefix,
        }
    }
}
