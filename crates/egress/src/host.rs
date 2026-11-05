use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde::Serialize;

/// A host after normalization.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Host {
    /// Lowercase ASCII labels with no trailing dot.
    Name(String),
    /// An IP in canonical form, so IPv4-mapped IPv6 is the IPv4 address.
    Ip(IpAddr),
}

impl std::fmt::Display for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Host::Name(name) => f.write_str(name),
            Host::Ip(IpAddr::V4(ip)) => write!(f, "{ip}"),
            Host::Ip(IpAddr::V6(ip)) => write!(f, "[{ip}]"),
        }
    }
}

/// Why a host was refused. Holds no input text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostError {
    #[error("host is empty")]
    Empty,
    #[error("host is too long")]
    TooLong,
    #[error("host contains whitespace")]
    Whitespace,
    #[error("host contains a control character")]
    ControlCharacter,
    #[error("host contains userinfo")]
    Userinfo,
    #[error("host is not ASCII; internationalized names must be in xn-- form")]
    NonAscii,
    #[error("host contains a character not allowed in a DNS name")]
    InvalidCharacter,
    #[error("host has an invalid DNS label")]
    InvalidLabel,
    #[error("host looks like an IP address but does not parse as one")]
    InvalidIpLiteral,
}

const MAX_RAW_HOST: usize = 300;
const MAX_NAME: usize = 253;
const MAX_LABEL: usize = 63;

/// Normalizes a host: lowercases, removes one trailing dot, and turns every IP
/// spelling (decimal, octal, hex, short forms, bracketed IPv6, IPv4-mapped)
/// into an [`IpAddr`]. Anything unclear is an error.
///
/// Non-ASCII is refused rather than converted. Correct IDNA needs Unicode
/// mapping tables, and getting those wrong would let `。` or lookalike
/// characters reach a different host than the rule named. `xn--` labels are
/// plain ASCII and compare literally.
pub fn normalize_host(input: &str) -> Result<Host, HostError> {
    if input.is_empty() {
        return Err(HostError::Empty);
    }
    if input.len() > MAX_RAW_HOST {
        return Err(HostError::TooLong);
    }
    for c in input.chars() {
        if c.is_whitespace() {
            return Err(HostError::Whitespace);
        }
        if c.is_control() {
            return Err(HostError::ControlCharacter);
        }
        if c == '@' {
            return Err(HostError::Userinfo);
        }
        if !c.is_ascii() {
            return Err(HostError::NonAscii);
        }
    }

    if let Some(inner) = input.strip_prefix('[') {
        let inner = inner.strip_suffix(']').ok_or(HostError::InvalidIpLiteral)?;
        let ip: Ipv6Addr = inner.parse().map_err(|_| HostError::InvalidIpLiteral)?;
        return Ok(Host::Ip(canonical_ip(IpAddr::V6(ip))));
    }
    if input.contains(':') {
        // Either bare IPv6 or `host:port`. The port belongs in its own field.
        let ip: Ipv6Addr = input.parse().map_err(|_| HostError::InvalidCharacter)?;
        return Ok(Host::Ip(canonical_ip(IpAddr::V6(ip))));
    }

    let lower = input.to_ascii_lowercase();
    let name = lower.strip_suffix('.').unwrap_or(&lower);
    if name.is_empty() {
        return Err(HostError::Empty);
    }
    if name.len() > MAX_NAME {
        return Err(HostError::TooLong);
    }
    let labels: Vec<&str> = name.split('.').collect();
    for label in &labels {
        check_label(label)?;
    }
    // Follow the URL standard's "ends in a number" test: such a host is an IP
    // or it is invalid. It is never a name, because resolvers and libraries
    // disagree on what it means.
    let last = labels[labels.len() - 1];
    if last.bytes().all(|b| b.is_ascii_digit()) || last.starts_with("0x") {
        let ip = parse_loose_ipv4(&labels).ok_or(HostError::InvalidIpLiteral)?;
        return Ok(Host::Ip(IpAddr::V4(ip)));
    }
    Ok(Host::Name(name.to_owned()))
}

fn check_label(label: &str) -> Result<(), HostError> {
    if label.is_empty() || label.len() > MAX_LABEL {
        return Err(HostError::InvalidLabel);
    }
    if !label
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(HostError::InvalidCharacter);
    }
    if label.starts_with('-') || label.ends_with('-') {
        return Err(HostError::InvalidLabel);
    }
    Ok(())
}

/// Parses the `inet_aton` forms: `a`, `a.b`, `a.b.c`, `a.b.c.d`, where each
/// part is decimal, octal with a leading `0`, or hex with a leading `0x`.
fn parse_loose_ipv4(parts: &[&str]) -> Option<Ipv4Addr> {
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut numbers = Vec::with_capacity(parts.len());
    for part in parts {
        numbers.push(parse_ipv4_number(part)?);
    }
    let (last, leading) = numbers.split_last()?;
    let mut value: u64 = 0;
    for (index, number) in leading.iter().enumerate() {
        if *number > 255 {
            return None;
        }
        value |= number << (8 * (3 - index));
    }
    let last_limit = 1u64 << (8 * (4 - leading.len()));
    if *last >= last_limit {
        return None;
    }
    value |= *last;
    Some(Ipv4Addr::from(u32::try_from(value).ok()?))
}

fn parse_ipv4_number(part: &str) -> Option<u64> {
    if let Some(hex) = part.strip_prefix("0x") {
        if hex.is_empty() {
            return Some(0);
        }
        return u64::from_str_radix(hex, 16).ok();
    }
    if part.len() > 1 && part.starts_with('0') {
        return u64::from_str_radix(&part[1..], 8).ok();
    }
    part.parse().ok()
}

/// Converts forms that denote an IPv4 address on the wire to that address:
/// IPv4-mapped (`::ffff:a.b.c.d`) and IPv4-compatible (`::a.b.c.d`).
pub fn canonical_ip(ip: IpAddr) -> IpAddr {
    let IpAddr::V6(v6) = ip else {
        return ip;
    };
    if let Some(v4) = v6.to_ipv4_mapped() {
        return IpAddr::V4(v4);
    }
    let s = v6.segments();
    if s[..6] == [0; 6] && v6 != Ipv6Addr::UNSPECIFIED && v6 != Ipv6Addr::LOCALHOST {
        return IpAddr::V4(low_v4(&s));
    }
    ip
}

fn low_v4(s: &[u16; 8]) -> Ipv4Addr {
    Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8)
}

/// What an address is, for deciding whether a sandbox may reach it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IpClass {
    Public,
    /// A cloud metadata endpoint. Always denied.
    Metadata,
    /// Always denied. The proxy runs on the host, so this reaches the host.
    Loopback,
    /// Always denied.
    LinkLocal,
    /// Always denied.
    Unspecified,
    /// Always denied.
    Multicast,
    /// Always denied.
    Broadcast,
    /// An IPv6 transition form that tunnels to an address not visible here.
    /// Always denied.
    Transition,
    /// RFC 1918, CGNAT or IPv6 ULA. Reachable only by an exact-IP rule.
    Private,
    /// Documentation, benchmarking and other non-public ranges. Reachable only
    /// by an exact-IP rule.
    Reserved,
}

impl IpClass {
    /// True if no rule of any kind can allow this class.
    pub fn is_always_denied(self) -> bool {
        matches!(
            self,
            IpClass::Metadata
                | IpClass::Loopback
                | IpClass::LinkLocal
                | IpClass::Unspecified
                | IpClass::Multicast
                | IpClass::Broadcast
                | IpClass::Transition
        )
    }
}

/// Classifies an address, looking through IPv6 forms that embed IPv4.
pub fn classify_ip(ip: IpAddr) -> IpClass {
    match canonical_ip(ip) {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => classify_v6(v6),
    }
}

const METADATA_V4: [Ipv4Addr; 5] = [
    Ipv4Addr::new(169, 254, 169, 254),
    Ipv4Addr::new(169, 254, 170, 2),
    Ipv4Addr::new(168, 63, 129, 16),
    Ipv4Addr::new(100, 100, 100, 200),
    Ipv4Addr::new(192, 0, 0, 192),
];

const METADATA_V6: [Ipv6Addr; 2] = [
    Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254),
    Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0xa9fe, 0xa9fe),
];

fn classify_v4(ip: Ipv4Addr) -> IpClass {
    if METADATA_V4.contains(&ip) {
        return IpClass::Metadata;
    }
    let [a, b, c, _] = ip.octets();
    match (a, b, c) {
        (0, _, _) => IpClass::Unspecified,
        (127, _, _) => IpClass::Loopback,
        (169, 254, _) => IpClass::LinkLocal,
        (10, _, _) | (192, 168, _) => IpClass::Private,
        (172, 16..=31, _) | (100, 64..=127, _) => IpClass::Private,
        (192, 0, 0) | (192, 0, 2) | (198, 51, 100) | (203, 0, 113) | (198, 18..=19, _) => {
            IpClass::Reserved
        }
        (224..=239, _, _) => IpClass::Multicast,
        (255, 255, 255) if ip == Ipv4Addr::BROADCAST => IpClass::Broadcast,
        (240..=255, _, _) => IpClass::Reserved,
        _ => IpClass::Public,
    }
}

fn classify_v6(ip: Ipv6Addr) -> IpClass {
    if METADATA_V6.contains(&ip) {
        return IpClass::Metadata;
    }
    if ip == Ipv6Addr::UNSPECIFIED {
        return IpClass::Unspecified;
    }
    if ip == Ipv6Addr::LOCALHOST {
        return IpClass::Loopback;
    }
    let s = ip.segments();
    // Forms that carry an IPv4 destination: a bad embedded address stays bad,
    // and any other is denied too, because the path it takes is not the one a
    // rule or a person reviewed.
    let embedded = if s[0] == 0x0064 && s[1] == 0xff9b {
        // NAT64, 64:ff9b::/96 and 64:ff9b:1::/48.
        Some(low_v4(&s))
    } else if s[..4] == [0; 4] && s[4] == 0xffff && s[5] == 0 {
        // SIIT, ::ffff:0:a.b.c.d.
        Some(low_v4(&s))
    } else if s[0] == 0x2002 {
        // 6to4, 2002:AABB:CCDD::/48.
        Some(Ipv4Addr::new(
            (s[1] >> 8) as u8,
            s[1] as u8,
            (s[2] >> 8) as u8,
            s[2] as u8,
        ))
    } else {
        None
    };
    if let Some(v4) = embedded {
        let class = classify_v4(v4);
        return if class.is_always_denied() {
            class
        } else {
            IpClass::Transition
        };
    }
    match s[0] {
        // Teredo.
        0x2001 if s[1] == 0 => IpClass::Transition,
        // Documentation.
        0x2001 if s[1] == 0x0db8 => IpClass::Reserved,
        0x2000..=0x3fff => IpClass::Public,
        0xfe80..=0xfebf => IpClass::LinkLocal,
        0xfc00..=0xfdff | 0xfec0..=0xfeff => IpClass::Private,
        0xff00..=0xffff => IpClass::Multicast,
        _ => IpClass::Reserved,
    }
}

/// True for names that always mean the local machine or a metadata service.
/// No rule can allow them. A resolved-address check would catch most of these
/// anyway, but refusing the name keeps the request off the resolver entirely.
pub fn is_reserved_name(name: &str) -> bool {
    const NAMES: [&str; 7] = [
        "localhost",
        "ip6-localhost",
        "ip6-loopback",
        "metadata",
        "metadata.google.internal",
        "metadata.goog",
        "instance-data",
    ];
    NAMES.contains(&name) || name.ends_with(".localhost") || name == "instance-data.ec2.internal"
}
