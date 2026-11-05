use crate::host::{is_reserved_name, normalize_host, Host, HostError};
use crate::http::{is_valid_method, is_valid_path_prefix, path_has_prefix};
use crate::policy::{Request, Scheme};

/// Client-supplied headers removed on every bound host, in lowercase.
///
/// Without this, a sandbox could send its own attacker-owned key through an
/// allowed provider and read stolen data back out of that account.
pub const ALWAYS_STRIPPED_HEADERS: [&str; 4] =
    ["authorization", "x-api-key", "api-key", "x-goog-api-key"];

/// How the secret becomes a header value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderFormat {
    /// The value as stored.
    Raw,
    /// `Bearer <value>`.
    Bearer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BindingError {
    #[error("a credential binding needs one exact DNS name, with no wildcard or IP")]
    NotAnExactName,
    #[error("invalid host in binding: {0}")]
    InvalidHost(HostError),
    #[error("this name always refers to the local machine or a metadata service")]
    ReservedName,
    #[error("header name must be a lowercase token")]
    InvalidHeader,
    #[error("secret name must match [A-Za-z_][A-Za-z0-9_]*")]
    InvalidSecretName,
    #[error("a binding must list at least one valid method")]
    InvalidMethods,
    #[error("invalid path prefix")]
    InvalidPathPrefix,
}

/// Ties one secret, by name, to one exact host. It holds no value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialBinding {
    host: String,
    port: u16,
    methods: Vec<String>,
    path_prefix: Option<String>,
    header: String,
    format: HeaderFormat,
    secret_name: String,
}

impl CredentialBinding {
    /// Creates a binding for `host` on port 443. A wildcard or IP is refused,
    /// so a credential can never be sent to a set of hosts.
    pub fn new(
        host: &str,
        methods: &[&str],
        path_prefix: Option<&str>,
        header: &str,
        format: HeaderFormat,
        secret_name: &str,
    ) -> Result<Self, BindingError> {
        if host.contains('*') {
            return Err(BindingError::NotAnExactName);
        }
        let host = match normalize_host(host).map_err(BindingError::InvalidHost)? {
            Host::Name(name) => name,
            Host::Ip(_) => return Err(BindingError::NotAnExactName),
        };
        if is_reserved_name(&host) {
            return Err(BindingError::ReservedName);
        }
        if methods.is_empty() || !methods.iter().all(|m| is_valid_method(m)) {
            return Err(BindingError::InvalidMethods);
        }
        if path_prefix.is_some_and(|p| !is_valid_path_prefix(p)) {
            return Err(BindingError::InvalidPathPrefix);
        }
        let header_ok = !header.is_empty()
            && header
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !header_ok {
            return Err(BindingError::InvalidHeader);
        }
        let mut name_chars = secret_name.chars();
        let name_ok = name_chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name_chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !name_ok {
            return Err(BindingError::InvalidSecretName);
        }
        Ok(Self {
            host,
            port: 443,
            methods: methods.iter().map(|m| (*m).to_owned()).collect(),
            path_prefix: path_prefix.map(str::to_owned),
            header: header.to_owned(),
            format,
            secret_name: secret_name.to_owned(),
        })
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn secret_name(&self) -> &str {
        &self.secret_name
    }
}

/// A header the proxy should add. The proxy looks the value up by name on the
/// host, and the value never passes through this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderInjection {
    pub header: String,
    pub format: HeaderFormat,
    pub secret_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    NotHttps,
    PortNotBound,
    MethodNotBound,
    PathNotBound,
    RedirectTarget,
}

/// What to do about a credential on this request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InjectionOutcome {
    /// The host has no binding. Leave its headers alone.
    NotBound,
    /// The host has a binding, but this request does not qualify. Do not add
    /// the credential, and still apply the strip list.
    Refused(RefusalReason),
    /// Apply the strip list, then add this header.
    Inject(HeaderInjection),
}

/// The plan for one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectionPlan {
    /// Client-supplied headers to remove, in lowercase. Empty if `NotBound`.
    pub strip_headers: Vec<String>,
    pub outcome: InjectionOutcome,
}

impl InjectionPlan {
    /// True if a client header must be removed. Compares ignoring case and
    /// treating `_` as `-`, because some servers read `x_api_key` as
    /// `x-api-key`.
    pub fn should_strip(&self, header: &str) -> bool {
        let folded: String = header
            .chars()
            .map(|c| {
                if c == '_' {
                    '-'
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect();
        self.strip_headers.contains(&folded)
    }

    fn not_bound() -> Self {
        Self {
            strip_headers: Vec::new(),
            outcome: InjectionOutcome::NotBound,
        }
    }
}

/// Plans credential handling for a request the policy has already allowed. This
/// does not replace [`crate::Policy::decide`].
///
/// A credential is added only if the normalized host equals a binding's host
/// exactly, and the scheme is https, and the port, method and path are all
/// bound. On any bound host, client-supplied credential headers are removed
/// whatever the outcome. With several bindings for a host, the first that
/// accepts the request is used.
pub fn plan_injection(request: &Request, bindings: &[CredentialBinding]) -> InjectionPlan {
    let Some(for_host) = bindings_for(&request.host, bindings) else {
        return InjectionPlan::not_bound();
    };
    InjectionPlan {
        strip_headers: strip_list(&for_host),
        outcome: match select_binding(request, &for_host) {
            Ok(binding) => InjectionOutcome::Inject(HeaderInjection {
                header: binding.header.clone(),
                format: binding.format,
                secret_name: binding.secret_name.clone(),
            }),
            Err(reason) => InjectionOutcome::Refused(reason),
        },
    }
}

/// Finds the first binding that accepts the request. On failure, reports how
/// far the closest binding got.
fn select_binding<'a>(
    request: &Request,
    for_host: &[&'a CredentialBinding],
) -> Result<&'a CredentialBinding, RefusalReason> {
    if Scheme::parse(&request.scheme) != Some(Scheme::Https) {
        return Err(RefusalReason::NotHttps);
    }
    let mut furthest = RefusalReason::PortNotBound;
    for binding in for_host {
        if binding.port != request.port {
            continue;
        }
        if furthest == RefusalReason::PortNotBound {
            furthest = RefusalReason::MethodNotBound;
        }
        if !binding.methods.contains(&request.method) {
            continue;
        }
        furthest = RefusalReason::PathNotBound;
        let path_ok = binding
            .path_prefix
            .as_deref()
            .is_none_or(|prefix| path_has_prefix(&request.path, prefix));
        if path_ok {
            return Ok(binding);
        }
    }
    Err(furthest)
}

/// Plans credential handling for a request the proxy is making because of a
/// redirect it followed. It never injects, even if the target is a bound host,
/// since the redirect's location was chosen by a remote server.
pub fn plan_redirect(target: &Request, bindings: &[CredentialBinding]) -> InjectionPlan {
    match bindings_for(&target.host, bindings) {
        Some(for_host) => InjectionPlan {
            strip_headers: strip_list(&for_host),
            outcome: InjectionOutcome::Refused(RefusalReason::RedirectTarget),
        },
        None => InjectionPlan::not_bound(),
    }
}

/// Finds the bindings for a host. A host that does not normalize to a name has
/// none, and the policy will already have denied it.
fn bindings_for<'a>(
    host: &str,
    bindings: &'a [CredentialBinding],
) -> Option<Vec<&'a CredentialBinding>> {
    let Ok(Host::Name(name)) = normalize_host(host) else {
        return None;
    };
    let for_host: Vec<&CredentialBinding> = bindings.iter().filter(|b| b.host == name).collect();
    if for_host.is_empty() {
        None
    } else {
        Some(for_host)
    }
}

fn strip_list(bindings: &[&CredentialBinding]) -> Vec<String> {
    let mut list: Vec<String> = ALWAYS_STRIPPED_HEADERS
        .iter()
        .map(|h| (*h).to_owned())
        .collect();
    for binding in bindings {
        if !list.contains(&binding.header) {
            list.push(binding.header.clone());
        }
    }
    list
}
