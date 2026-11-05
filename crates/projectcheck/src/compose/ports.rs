//! Published ports, in short and long syntax.

use std::net::IpAddr;

use serde_norway::Value;

use super::interp::{interpolate, split_outside_vars};
use super::{
    key_text, scalar_text, show, Ctx, FindingKind, Interface, Port, Severity, UNKNOWN_VALUE,
};
use crate::clean;

pub(super) fn analyze(service: &str, value: &Value, ctx: &mut Ctx) -> Vec<Port> {
    let items: Vec<&Value> = match value {
        Value::Null => return Vec::new(),
        Value::Sequence(items) => items.iter().collect(),
        Value::String(_) | Value::Number(_) => {
            ctx.unrecognized(Some(service), "`ports` is not a list");
            vec![value]
        }
        _ => {
            ctx.unrecognized(Some(service), "`ports` has an unknown shape");
            return Vec::new();
        }
    };
    let mut ports = Vec::new();
    for item in items {
        let parsed = match item {
            Value::String(text) => short_form(service, text, ctx),
            Value::Number(number) if number.is_u64() => {
                short_form(service, &number.to_string(), ctx)
            }
            Value::Mapping(_) => long_form(service, item, ctx),
            other => {
                ctx.unrecognized(
                    Some(service),
                    &format!("`ports` has an entry of an unknown shape: {}", show(other)),
                );
                None
            }
        };
        if let Some(port) = parsed {
            report(service, &port, ctx);
            ports.push(port);
        }
    }
    ports
}

/// Parses `[ip:][host:]target[/protocol]`. Variables are applied first, because
/// one variable can hold several of those parts.
fn short_form(service: &str, raw: &str, ctx: &mut Ctx) -> Option<Port> {
    let text = interpolate(raw);
    let (spec, protocol) = match text.value.rsplit_once('/') {
        Some((spec, protocol)) => (spec, protocol.to_string()),
        None => (text.value.as_str(), "tcp".to_string()),
    };
    let fields: Vec<&str> = spec.split(':').collect();
    let (ip, host, target) = match fields.as_slice() {
        [target] => (None, None, *target),
        [host, target] => (None, Some(*host), *target),
        [.., host, target] => {
            let keep = spec.len() - host.len() - target.len() - 2;
            (Some(&spec[..keep]), Some(*host), *target)
        }
        [] => (None, None, ""),
    };

    let bad = |ctx: &mut Ctx, what: &str| {
        ctx.unrecognized(
            Some(service),
            &format!("port `{}` has an unrecognized {what}", clean(raw)),
        );
    };
    if !matches!(protocol.as_str(), "tcp" | "udp" | "sctp") {
        bad(ctx, "protocol");
    }
    let target_range = parse_range(target);
    if target_range.is_none() {
        bad(ctx, "container port");
    }
    let host = host.filter(|h| !h.is_empty());
    if let Some(host) = host {
        match (parse_range(host), target_range) {
            (None, _) if !host.contains(UNKNOWN_VALUE) => bad(ctx, "host port"),
            (Some((h1, h2)), Some((t1, t2))) => {
                let (host_size, target_size) = (h2 - h1, t2 - t1);
                if target_size != 0 && host_size != target_size {
                    bad(ctx, "pair of port ranges");
                }
            }
            _ => {}
        }
    }

    let raw_fields = split_outside_vars(raw.rsplit_once('/').map_or(raw, |(spec, _)| spec), ':');
    // Fields that line up with the written text show whether a variable sets
    // the address. When they do not line up, one variable holds several parts.
    let ip_is_dynamic = if raw_fields.len() == fields.len() {
        raw_fields[..raw_fields.len().saturating_sub(2)]
            .iter()
            .any(|f| f.contains('$'))
    } else {
        true
    };
    let interface = classify_interface(ip);
    if ip_is_dynamic && matches!(interface, Interface::Loopback | Interface::Specific(_)) {
        ctx.push(
            Severity::Medium,
            FindingKind::PortUnknownInterface,
            Some(service),
            &format!(
                "port `{}` takes its address from a variable, which can change it to all interfaces",
                clean(raw)
            ),
        );
    }
    Some(Port {
        target: target_range.map(|r| r.0),
        target_end: target_range.and_then(|(start, end)| (end != start).then_some(end)),
        published: host.map(clean),
        interface,
        protocol: clean(&protocol),
        dynamic: text.dynamic,
    })
}

fn long_form(service: &str, value: &Value, ctx: &mut Ctx) -> Option<Port> {
    let map = value.as_mapping()?;
    let mut target = None;
    let mut published = None;
    let mut host_ip = None;
    let mut protocol = "tcp".to_string();
    let mut dynamic = false;
    let mut host_ip_seen = false;
    let mut host_ip_dynamic = false;
    for (key, item) in map {
        let key = key_text(key);
        match key.as_str() {
            "target" => {
                let text = scalar_text(item).map(|t| interpolate(&t));
                dynamic |= text.as_ref().is_some_and(|t| t.dynamic);
                target = text.and_then(|t| parse_range(&t.value));
                if target.is_none() {
                    ctx.unrecognized(
                        Some(service),
                        &format!(
                            "long-form port has an unrecognized `target`: {}",
                            show(item)
                        ),
                    );
                }
            }
            "published" => match scalar_text(item).map(|t| interpolate(&t)) {
                Some(text) => {
                    dynamic |= text.dynamic;
                    if parse_range(&text.value).is_none() && !text.unresolved {
                        ctx.unrecognized(
                            Some(service),
                            &format!(
                                "long-form port has an unrecognized `published`: {}",
                                show(item)
                            ),
                        );
                    }
                    published = Some(text.value);
                }
                None if item.is_null() => {}
                None => ctx.unrecognized(
                    Some(service),
                    "long-form port has `published` of an unknown shape",
                ),
            },
            "host_ip" => {
                host_ip_seen = true;
                match item.as_str().map(interpolate) {
                    Some(text) => {
                        dynamic |= text.dynamic;
                        host_ip_dynamic = text.dynamic;
                        host_ip = Some(text.value);
                    }
                    None => ctx.unrecognized(
                        Some(service),
                        "long-form port has `host_ip` that is not text",
                    ),
                }
            }
            "protocol" => match item.as_str() {
                Some(text) => {
                    protocol = text.to_string();
                    if !matches!(text, "tcp" | "udp" | "sctp") {
                        ctx.unrecognized(
                            Some(service),
                            &format!(
                                "long-form port has an unrecognized protocol: {}",
                                show(item)
                            ),
                        );
                    }
                }
                None => ctx.unrecognized(
                    Some(service),
                    "long-form port has a protocol that is not text",
                ),
            },
            "mode" | "name" | "app_protocol" => {}
            other if other.starts_with("x-") => {}
            other => ctx.unrecognized(
                Some(service),
                &format!("long-form port has unknown field `{other}`"),
            ),
        }
    }
    if !map.contains_key("target") {
        ctx.unrecognized(Some(service), "long-form port has no `target`");
    }
    let interface = if host_ip_seen && host_ip.is_none() {
        Interface::Unknown
    } else {
        classify_interface(host_ip.as_deref())
    };
    if host_ip_dynamic && matches!(interface, Interface::Loopback | Interface::Specific(_)) {
        ctx.push(
            Severity::Medium,
            FindingKind::PortUnknownInterface,
            Some(service),
            "long-form port takes `host_ip` from a variable, which can change it to all interfaces",
        );
    }
    Some(Port {
        target: target.map(|r: (u16, u16)| r.0),
        target_end: target.and_then(|(start, end)| (end != start).then_some(end)),
        published: published.map(|p| clean(&p)),
        interface,
        protocol: clean(&protocol),
        dynamic,
    })
}

/// A written address that is not a valid IP is `Unknown`, never loopback, so
/// that `localhost` or `127.1` cannot pass as a restriction.
fn classify_interface(ip: Option<&str>) -> Interface {
    let Some(ip) = ip else { return Interface::All };
    if ip.is_empty() {
        return Interface::All;
    }
    let bare = ip
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(ip);
    match bare.parse::<IpAddr>() {
        Ok(addr) if addr.is_unspecified() => Interface::All,
        Ok(addr) if addr.is_loopback() => Interface::Loopback,
        Ok(addr) => Interface::Specific(addr.to_string()),
        Err(_) => Interface::Unknown,
    }
}

/// Parses `80` or `80-90`. Digits only, and port 0 is not valid.
fn parse_range(text: &str) -> Option<(u16, u16)> {
    let parse_one = |part: &str| -> Option<u16> {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse::<u16>().ok().filter(|port| *port != 0)
    };
    match text.split_once('-') {
        None => parse_one(text).map(|port| (port, port)),
        Some((start, end)) => {
            let (start, end) = (parse_one(start)?, parse_one(end)?);
            (start <= end).then_some((start, end))
        }
    }
}

fn report(service: &str, port: &Port, ctx: &mut Ctx) {
    let label = match (port.target, &port.published) {
        (Some(target), Some(host)) => format!("{host} to container port {target}"),
        (Some(target), None) => format!("a Docker-chosen port to container port {target}"),
        (None, Some(host)) => format!("{host} to an unknown container port"),
        (None, None) => "an unknown port".to_string(),
    };
    let (severity, kind, place) = match &port.interface {
        Interface::All => (
            Severity::High,
            FindingKind::PortAllInterfaces,
            "on all interfaces".to_string(),
        ),
        Interface::Loopback => (
            Severity::Info,
            FindingKind::PortLoopback,
            "on loopback only".to_string(),
        ),
        Interface::Specific(addr) => (
            Severity::Medium,
            FindingKind::PortSpecificInterface,
            format!("on {addr}"),
        ),
        Interface::Unknown => (
            Severity::High,
            FindingKind::PortUnknownInterface,
            "on an interface that cannot be determined, so treat it as all interfaces".to_string(),
        ),
    };
    ctx.push(
        severity,
        kind,
        Some(service),
        &format!("publishes {label} {place}"),
    );
}
