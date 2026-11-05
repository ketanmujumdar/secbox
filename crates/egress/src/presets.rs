//! Ready-made rules.

use crate::host::Host;
use crate::policy::{HostPattern, PolicyError, Rule, RuleSpec};

/// npm registry reads over https. `PUT` and `DELETE`, which publish and
/// unpublish, are denied, and so is `POST`, which `npm audit` and login use.
pub fn registry_read_only() -> Rule {
    Rule::from_parts(
        "npm-registry-read-only",
        HostPattern::Exact("registry.npmjs.org".to_owned()),
        &["GET", "HEAD"],
        None,
    )
}

/// An AI provider API: one exact host, one method and one path prefix, over
/// https on port 443. The host cannot be a wildcard or an IP.
pub fn ai_provider(host: &str, method: &str, path_prefix: &str) -> Result<Rule, PolicyError> {
    if host.contains('*') {
        return Err(PolicyError::InvalidWildcard);
    }
    let normalized = match crate::host::normalize_host(host) {
        Ok(Host::Name(name)) => name,
        Ok(Host::Ip(_)) => {
            return Err(PolicyError::InvalidHost(crate::HostError::InvalidCharacter))
        }
        Err(error) => return Err(PolicyError::InvalidHost(error)),
    };
    Rule::new(RuleSpec {
        name: Some(format!("ai-provider:{normalized}")),
        host: normalized,
        ports: vec![443],
        methods: vec![method.to_owned()],
        schemes: None,
        path_prefix: Some(path_prefix.to_owned()),
    })
}

/// The Anthropic Messages API: `POST` under `/v1/messages`.
pub fn anthropic_messages() -> Rule {
    Rule::from_parts(
        "ai-provider:api.anthropic.com",
        HostPattern::Exact("api.anthropic.com".to_owned()),
        &["POST"],
        Some("/v1/messages".to_owned()),
    )
}
