//! The egress decision engine. It is pure: it opens no sockets, resolves no
//! names and reads no files. A proxy calls it and enforces the answers.
//!
//! The expected order for one request through the proxy:
//!
//! 1. [`Policy::decide`]. Stop on deny and log a [`BlockedEvent`].
//! 2. Resolve the name and call [`Policy::check_resolved`] for each address.
//!    Connect to the address that was checked, without resolving again.
//! 3. [`plan_injection`], then apply its strip list and injection.
//!
//! Allow rules have no precedence and there are no deny rules, so rule order
//! does not affect a decision.

mod event;
mod host;
mod http;
mod inject;
mod policy;
pub mod presets;

pub use event::BlockedEvent;
pub use host::{
    canonical_ip, classify_ip, is_reserved_name, normalize_host, Host, HostError, IpClass,
};
pub use inject::{
    plan_injection, plan_redirect, BindingError, CredentialBinding, HeaderFormat, HeaderInjection,
    InjectionOutcome, InjectionPlan, RefusalReason, ALWAYS_STRIPPED_HEADERS,
};
pub use policy::{
    classify_change, ChangeKind, Decision, DenyReason, Policy, PolicyChange, PolicyError, Request,
    Rule, RuleSpec, Scheme, MAX_POLICY_FILE_BYTES,
};
