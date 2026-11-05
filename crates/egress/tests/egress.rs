use std::net::IpAddr;

use egress::presets::{ai_provider, anthropic_messages, registry_read_only};
use egress::*;

fn req(host: &str, port: u16, method: &str, path: &str, scheme: &str) -> Request {
    Request {
        host: host.to_owned(),
        port,
        method: method.to_owned(),
        path: path.to_owned(),
        scheme: scheme.to_owned(),
    }
}

fn get(host: &str) -> Request {
    req(host, 443, "GET", "/", "https")
}

fn rule(host: &str) -> Rule {
    Rule::new(RuleSpec {
        name: None,
        host: host.to_owned(),
        ports: vec![443],
        methods: vec!["GET".to_owned()],
        schemes: None,
        path_prefix: None,
    })
    .unwrap()
}

fn npm() -> Policy {
    Policy::new(vec![registry_read_only()])
}

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

fn denied(d: &Decision) -> &DenyReason {
    d.deny_reason().expect("expected a denial")
}

// Default deny and host matching

#[test]
fn empty_policy_denies_everything() {
    let p = Policy::deny_all();
    assert_eq!(
        denied(&p.decide(&get("example.com"))),
        &DenyReason::NoMatchingRule
    );
}

#[test]
fn registry_hostile_host_spellings() {
    let p = npm();
    for allowed in [
        "registry.npmjs.org",
        "registry.npmjs.org.",
        "REGISTRY.NPMJS.ORG",
        "Registry.NpmJs.Org.",
    ] {
        let d = p.decide(&get(allowed));
        assert!(d.is_allowed(), "{allowed}: {d:?}");
        assert_eq!(
            d,
            Decision::Allow {
                rule: "npm-registry-read-only".to_owned(),
                host: "registry.npmjs.org".to_owned()
            }
        );
    }
    for denied_host in [
        "evil-npmjs.org",
        "registry.npmjs.org.evil.com",
        "evilregistry.npmjs.org",
        "npmjs.org",
        "sub.registry.npmjs.org",
    ] {
        assert!(!p.decide(&get(denied_host)).is_allowed(), "{denied_host}");
    }
}

#[test]
fn wildcard_matches_only_on_dot_boundary() {
    let p = Policy::new(vec![rule("*.npmjs.org")]);
    assert!(p.decide(&get("registry.npmjs.org")).is_allowed());
    assert!(p.decide(&get("a.b.npmjs.org")).is_allowed());
    assert!(p.decide(&get("A.NPMJS.ORG.")).is_allowed());
    for host in [
        "evil-npmjs.org",
        "npmjs.org",
        "npmjs.org.evil.com",
        "xnpmjs.org",
        ".npmjs.org",
    ] {
        assert!(!p.decide(&get(host)).is_allowed(), "{host}");
    }
}

#[test]
fn wildcard_forms_are_restricted() {
    for bad in [
        "*",
        "*.com",
        "*npmjs.org",
        "a*.npmjs.org",
        "*.*.npmjs.org",
        "registry.*.org",
        "*.",
        "*.1.1.1.1",
        "*.127.0.0.1",
        "*.169.254.169.254",
    ] {
        let err = Rule::new(RuleSpec {
            name: None,
            host: bad.to_owned(),
            ports: vec![443],
            methods: vec!["GET".to_owned()],
            schemes: None,
            path_prefix: None,
        });
        assert!(err.is_err(), "{bad} should be refused");
    }
}

#[test]
fn host_normalization_fails_closed() {
    for (input, expected) in [
        ("", HostError::Empty),
        (".", HostError::Empty),
        ("a..b.com", HostError::InvalidLabel),
        ("a.com..", HostError::InvalidLabel),
        (".a.com", HostError::InvalidLabel),
        ("user@a.com", HostError::Userinfo),
        ("user:pw@a.com", HostError::Userinfo),
        ("a.com@evil.com", HostError::Userinfo),
        ("a .com", HostError::Whitespace),
        (" a.com", HostError::Whitespace),
        ("a.com\n", HostError::Whitespace),
        ("a.com\r\nHost: evil", HostError::Whitespace),
        ("a\u{3000}.com", HostError::Whitespace),
        ("a\u{0}.com", HostError::ControlCharacter),
        ("a\u{7f}.com", HostError::ControlCharacter),
        ("münchen.de", HostError::NonAscii),
        ("registry\u{3002}npmjs\u{3002}org", HostError::NonAscii),
        ("rеgistry.npmjs.org", HostError::NonAscii),
        ("ｒegistry.npmjs.org", HostError::NonAscii),
        ("a.com:443", HostError::InvalidCharacter),
        ("a.com/path", HostError::InvalidCharacter),
        ("a.com?x", HostError::InvalidCharacter),
        ("a.com#x", HostError::InvalidCharacter),
        ("a%2ecom", HostError::InvalidCharacter),
        ("a_b.com", HostError::InvalidCharacter),
        ("*.a.com", HostError::InvalidCharacter),
        ("-a.com", HostError::InvalidLabel),
        ("a-.com", HostError::InvalidLabel),
    ] {
        assert_eq!(normalize_host(input), Err(expected), "input {input:?}");
    }
    assert_eq!(
        normalize_host(&format!("{}.com", "a".repeat(64))),
        Err(HostError::InvalidLabel)
    );
    assert_eq!(normalize_host(&"a.".repeat(140)), Err(HostError::TooLong));
    assert_eq!(
        normalize_host("XN--MNCHEN-3YA.de."),
        Ok(Host::Name("xn--mnchen-3ya.de".to_owned()))
    );
}

#[test]
fn hostile_hosts_are_denied_by_decide() {
    let p = Policy::new(vec![registry_read_only(), rule("*.npmjs.org")]);
    for host in [
        "user@registry.npmjs.org",
        "registry.npmjs.org@evil.com",
        "registry.npmjs.org evil.com",
        "registry.npmjs.org\t",
        "regіstry.npmjs.org",
        "registry。npmjs。org",
        "registry.npmjs.org:443",
    ] {
        let d = p.decide(&get(host));
        assert!(
            matches!(denied(&d), DenyReason::InvalidHost(_)),
            "{host:?}: {d:?}"
        );
    }
}

// IP literals

#[test]
fn every_ip_spelling_parses_to_the_same_address() {
    let loopback = Host::Ip(ip("127.0.0.1"));
    for spelling in [
        "127.0.0.1",
        "127.0.0.1.",
        "127.1",
        "127.0.1",
        "2130706433",
        "0x7f.1",
        "0x7F.1",
        "0x7f000001",
        "0X7F000001",
        "017700000001",
        "0177.0.0.1",
        "0177.1",
        "0x7f.0.0.1",
        "00000177.0.0.1",
        "[::ffff:127.0.0.1]",
        "[::ffff:7f00:1]",
        "::ffff:127.0.0.1",
        "[::127.0.0.1]",
        "[0:0:0:0:0:ffff:7f00:0001]",
    ] {
        assert_eq!(
            normalize_host(spelling).as_ref(),
            Ok(&loopback),
            "{spelling}"
        );
    }
    assert_eq!(normalize_host("[::1]"), Ok(Host::Ip(ip("::1"))));
    assert_eq!(normalize_host("0"), Ok(Host::Ip(ip("0.0.0.0"))));
    assert_eq!(normalize_host("0x"), Ok(Host::Ip(ip("0.0.0.0"))));
    assert_eq!(normalize_host("1.2.3"), Ok(Host::Ip(ip("1.2.0.3"))));
    assert_eq!(normalize_host("1.2.256"), Ok(Host::Ip(ip("1.2.1.0"))));
}

#[test]
fn malformed_numeric_hosts_are_errors_not_names() {
    for bad in [
        "256.0.0.1",
        "1.2.3.256",
        "1.2.3.4.5",
        "4294967296",
        "99999999999999999999999",
        "0x100000000",
        "08.0.0.1",
        "0189",
        "1.2.65536",
        "foo.123",
        "foo.0x7f",
        "0xzz",
        "a.0xzz",
        "[127.0.0.1]",
        "[::1",
        "::1]",
        "[::1]:443",
        "[::1]x",
        "[fe80::1%25en0]",
        "fe80::1%en0",
    ] {
        assert!(normalize_host(bad).is_err(), "{bad} should not parse");
    }
    // A numeric label that is not the last one is still a name.
    assert_eq!(
        normalize_host("1.2.3.example.com"),
        Ok(Host::Name("1.2.3.example.com".to_owned()))
    );
}

#[test]
fn loopback_and_metadata_are_denied_in_every_form_even_with_broad_rules() {
    let p = Policy::new(vec![
        registry_read_only(),
        rule("*.npmjs.org"),
        rule("*.amazonaws.com"),
    ]);
    for host in [
        "127.0.0.1",
        "0x7f.1",
        "2130706433",
        "017700000001",
        "[::1]",
        "[::ffff:127.0.0.1]",
        "0.0.0.0",
        "0",
        "[::]",
    ] {
        let d = p.decide(&get(host));
        assert!(
            matches!(denied(&d), DenyReason::BlockedAddress(_)),
            "{host}: {d:?}"
        );
    }
    for host in [
        "169.254.169.254",
        "2852039166",
        "0xa9fea9fe",
        "0251.0376.0251.0376",
        "169.254.43518",
        "[::ffff:169.254.169.254]",
        "[::ffff:a9fe:a9fe]",
        "[fd00:ec2::254]",
        "[FD00:0EC2:0:0:0:0:0:254]",
        "[::169.254.169.254]",
        "168.63.129.16",
        "100.100.100.200",
    ] {
        assert_eq!(
            denied(&p.decide(&get(host))),
            &DenyReason::BlockedAddress(IpClass::Metadata),
            "{host}"
        );
    }
}

#[test]
fn ipv6_transition_forms_cannot_hide_a_blocked_v4() {
    assert_eq!(classify_ip(ip("2002:7f00:1::")), IpClass::Loopback);
    assert_eq!(classify_ip(ip("2002:a9fe:a9fe::")), IpClass::Metadata);
    assert_eq!(classify_ip(ip("64:ff9b::7f00:1")), IpClass::Loopback);
    assert_eq!(classify_ip(ip("64:ff9b::a9fe:a9fe")), IpClass::Metadata);
    assert_eq!(classify_ip(ip("::ffff:0:7f00:1")), IpClass::Loopback);
    // A private v4 inside a transition form is denied outright.
    assert_eq!(classify_ip(ip("2002:0a00:1::")), IpClass::Transition);
    assert_eq!(classify_ip(ip("64:ff9b::808:808")), IpClass::Transition);
    assert_eq!(classify_ip(ip("2001:0:1::1")), IpClass::Transition);
    assert!(classify_ip(ip("2002:0a00:1::")).is_always_denied());
}

#[test]
fn classification_table() {
    for (addr, class) in [
        ("10.0.0.1", IpClass::Private),
        ("172.16.0.1", IpClass::Private),
        ("172.31.255.255", IpClass::Private),
        ("192.168.1.1", IpClass::Private),
        ("100.64.0.1", IpClass::Private),
        ("fc00::1", IpClass::Private),
        ("fd12::1", IpClass::Private),
        ("fec0::1", IpClass::Private),
        ("127.255.0.1", IpClass::Loopback),
        ("169.254.1.1", IpClass::LinkLocal),
        ("fe80::1", IpClass::LinkLocal),
        ("feb0::1", IpClass::LinkLocal),
        ("224.0.0.1", IpClass::Multicast),
        ("ff02::1", IpClass::Multicast),
        ("255.255.255.255", IpClass::Broadcast),
        ("240.0.0.1", IpClass::Reserved),
        ("192.0.2.1", IpClass::Reserved),
        ("2001:db8::1", IpClass::Reserved),
        ("100::1", IpClass::Reserved),
        ("172.32.0.1", IpClass::Public),
        ("172.15.0.1", IpClass::Public),
        ("100.128.0.1", IpClass::Public),
        ("8.8.8.8", IpClass::Public),
        ("2606:4700::1111", IpClass::Public),
    ] {
        assert_eq!(classify_ip(ip(addr)), class, "{addr}");
    }
}

#[test]
fn ip_literals_need_a_rule_naming_that_exact_ip() {
    let none = Policy::new(vec![rule("*.example.com")]);
    assert_eq!(
        denied(&none.decide(&get("8.8.8.8"))),
        &DenyReason::IpLiteralNotAllowed
    );
    assert_eq!(
        denied(&none.decide(&get("10.0.0.1"))),
        &DenyReason::BlockedAddress(IpClass::Private)
    );

    let p = Policy::new(vec![rule("8.8.8.8"), rule("10.1.2.3")]);
    assert!(p.decide(&get("8.8.8.8")).is_allowed());
    // Every spelling of the same address is the same address.
    assert!(p.decide(&get("0x08080808")).is_allowed());
    assert!(p.decide(&get("[::ffff:8.8.8.8]")).is_allowed());
    assert!(p.decide(&get("10.1.2.3")).is_allowed());
    assert!(!p.decide(&get("8.8.4.4")).is_allowed());
    assert!(!p.decide(&get("10.1.2.4")).is_allowed());
    assert_eq!(
        p.decide(&get("0x08080808")),
        Decision::Allow {
            rule: "8.8.8.8".to_owned(),
            host: "8.8.8.8".to_owned()
        }
    );
}

#[test]
fn no_rule_can_name_an_always_denied_address_or_name() {
    for host in [
        "127.0.0.1",
        "0x7f.1",
        "[::1]",
        "169.254.169.254",
        "[::ffff:169.254.169.254]",
        "[fd00:ec2::254]",
        "0.0.0.0",
        "localhost",
        "LOCALHOST.",
        "foo.localhost",
        "metadata.google.internal",
    ] {
        let spec = RuleSpec {
            name: None,
            host: host.to_owned(),
            ports: vec![80, 443],
            methods: vec!["GET".to_owned()],
            schemes: None,
            path_prefix: None,
        };
        assert!(
            matches!(
                Rule::new(spec),
                Err(PolicyError::BlockedAddress(_) | PolicyError::ReservedName)
            ),
            "{host}"
        );
    }
    assert!(matches!(
        Rule::new(RuleSpec {
            name: None,
            host: "*.foo.localhost".to_owned(),
            ports: vec![443],
            methods: vec!["GET".to_owned()],
            schemes: None,
            path_prefix: None,
        }),
        Err(PolicyError::ReservedName)
    ));
}

#[test]
fn reserved_names_are_denied_even_under_a_matching_wildcard() {
    let p = Policy::new(vec![rule("*.google.internal"), rule("*.ec2.internal")]);
    for host in [
        "metadata.google.internal",
        "METADATA.GOOGLE.INTERNAL.",
        "instance-data.ec2.internal",
        "localhost",
        "a.localhost",
    ] {
        assert_eq!(
            denied(&p.decide(&get(host))),
            &DenyReason::ReservedName,
            "{host}"
        );
    }
}

// DNS rebinding

#[test]
fn check_resolved_denies_rebinding_to_non_public_addresses() {
    let p = npm();
    for addr in [
        "127.0.0.1",
        "::1",
        "::ffff:127.0.0.1",
        "169.254.169.254",
        "::ffff:169.254.169.254",
        "fd00:ec2::254",
        "10.0.0.1",
        "::ffff:10.0.0.1",
        "172.16.0.1",
        "192.168.1.1",
        "100.64.0.1",
        "fc00::1",
        "fe80::1",
        "0.0.0.0",
        "::",
        "224.0.0.1",
        "192.0.2.1",
        "2002:7f00:1::",
        "64:ff9b::a00:1",
    ] {
        let d = p.check_resolved("registry.npmjs.org", ip(addr));
        assert!(
            matches!(denied(&d), DenyReason::ResolvedAddressBlocked(_)),
            "{addr}: {d:?}"
        );
    }
    assert!(p
        .check_resolved("REGISTRY.npmjs.org.", ip("104.16.0.1"))
        .is_allowed());
    assert!(p
        .check_resolved("registry.npmjs.org", ip("2606:4700::1111"))
        .is_allowed());
}

#[test]
fn check_resolved_also_requires_the_host_to_be_allowed() {
    let p = npm();
    assert_eq!(
        denied(&p.check_resolved("evil.com", ip("8.8.8.8"))),
        &DenyReason::NoMatchingRule
    );
    assert_eq!(
        denied(&p.check_resolved("localhost", ip("8.8.8.8"))),
        &DenyReason::ReservedName
    );
    assert!(matches!(
        denied(&p.check_resolved("a b", ip("8.8.8.8"))),
        DenyReason::InvalidHost(_)
    ));
}

#[test]
fn exact_private_ip_rule_does_not_let_a_name_rebind_to_it() {
    let p = Policy::new(vec![registry_read_only(), rule("10.1.2.3")]);
    assert!(!p
        .check_resolved("registry.npmjs.org", ip("10.1.2.3"))
        .is_allowed());
    assert!(p.check_resolved("10.1.2.3", ip("10.1.2.3")).is_allowed());
    assert_eq!(
        denied(&p.check_resolved("10.1.2.3", ip("10.1.2.4"))),
        &DenyReason::ResolvedAddressMismatch
    );
}

// Methods, ports, schemes and paths

#[test]
fn registry_is_read_only() {
    let p = npm();
    assert!(p
        .decide(&req("registry.npmjs.org", 443, "GET", "/left-pad", "https"))
        .is_allowed());
    assert!(p
        .decide(&req(
            "registry.npmjs.org",
            443,
            "HEAD",
            "/left-pad",
            "https"
        ))
        .is_allowed());
    for method in [
        "PUT", "DELETE", "POST", "PATCH", "OPTIONS", "CONNECT", "TRACE",
    ] {
        assert_eq!(
            denied(&p.decide(&req("registry.npmjs.org", 443, method, "/x", "https"))),
            &DenyReason::MethodNotAllowed {
                rule: "npm-registry-read-only".to_owned()
            },
            "{method}"
        );
    }
}

#[test]
fn malformed_methods_are_invalid_not_normalized() {
    let p = npm();
    for method in ["get", "Get", "", "GET ", "G\nET", "PUT\r\nX: y", "G-T"] {
        assert_eq!(
            denied(&p.decide(&req("registry.npmjs.org", 443, method, "/", "https"))),
            &DenyReason::InvalidMethod,
            "{method:?}"
        );
    }
}

#[test]
fn port_and_scheme_are_enforced() {
    let p = npm();
    assert!(matches!(
        denied(&p.decide(&req("registry.npmjs.org", 8443, "GET", "/", "https"))),
        DenyReason::PortNotAllowed { .. }
    ));
    assert_eq!(
        denied(&p.decide(&req("registry.npmjs.org", 0, "GET", "/", "https"))),
        &DenyReason::InvalidPort
    );
    assert!(matches!(
        denied(&p.decide(&req("registry.npmjs.org", 443, "GET", "/", "http"))),
        DenyReason::SchemeNotAllowed { .. }
    ));
    assert!(p
        .decide(&req("registry.npmjs.org", 443, "GET", "/", "HTTPS"))
        .is_allowed());
    for scheme in ["ftp", "", "file", "https ", "gopher"] {
        assert_eq!(
            denied(&p.decide(&req("registry.npmjs.org", 443, "GET", "/", scheme))),
            &DenyReason::InvalidScheme,
            "{scheme:?}"
        );
    }
}

#[test]
fn request_target_must_be_origin_form() {
    let p = npm();
    for path in [
        "",
        "*",
        "http://evil.com/",
        "x",
        "/a b",
        "/a\r\nHost: evil",
        "/a\tb",
        "/\u{0}",
        "/é",
    ] {
        assert_eq!(
            denied(&p.decide(&req("registry.npmjs.org", 443, "GET", path, "https"))),
            &DenyReason::InvalidPath,
            "{path:?}"
        );
    }
}

#[test]
fn ai_provider_preset_binds_host_method_and_path() {
    let p = Policy::new(vec![anthropic_messages()]);
    let host = "api.anthropic.com";
    for path in [
        "/v1/messages",
        "/v1/messages/",
        "/v1/messages/count_tokens",
        "/v1/messages?beta=true",
    ] {
        assert!(
            p.decide(&req(host, 443, "POST", path, "https"))
                .is_allowed(),
            "{path}"
        );
    }
    for path in [
        "/v1/messagesx",
        "/v1/files",
        "/",
        "/v1/messages/../files",
        "/v1/messages/%2e%2e/files",
        "/v1/messages/%2E%2E/files",
        "/v1/messages/%252e%252e/files",
        "/v1/messages/..%2ffiles",
        "/v1/messages%2f..%2ffiles",
        "/v1/messages/./x",
        "//v1/messages",
        "/v1//messages",
        "/v1/messages//x",
        "/v1/messages\\..\\files",
        "/v1/messages/..;/files",
        "/v1/messages;x",
        "/v1/messages#/../files",
    ] {
        assert!(
            matches!(
                denied(&p.decide(&req(host, 443, "POST", path, "https"))),
                DenyReason::PathNotAllowed { .. }
            ),
            "{path}"
        );
    }
    assert!(!p
        .decide(&req(host, 443, "GET", "/v1/messages", "https"))
        .is_allowed());
    assert!(!p
        .decide(&req(
            "evil.anthropic.com",
            443,
            "POST",
            "/v1/messages",
            "https"
        ))
        .is_allowed());
}

#[test]
fn ai_provider_preset_validates_its_arguments() {
    let r = ai_provider("API.Example.com.", "POST", "/v1/chat").unwrap();
    assert_eq!(r.label(), "ai-provider:api.example.com");
    assert!(ai_provider("*.example.com", "POST", "/v1").is_err());
    assert!(ai_provider("8.8.8.8", "POST", "/v1").is_err());
    assert!(ai_provider("0x7f.1", "POST", "/v1").is_err());
    assert!(ai_provider("localhost", "POST", "/v1").is_err());
    assert!(ai_provider("a.com", "post", "/v1").is_err());
    assert!(ai_provider("a.com", "CONNECT", "/v1").is_err());
    assert!(ai_provider("a.com", "POST", "v1").is_err());
    assert!(ai_provider("a.com", "POST", "/v1/../x").is_err());
    assert!(ai_provider("a.com", "POST", "/v1?x").is_err());
}

#[test]
fn prefix_with_trailing_slash() {
    let r = ai_provider("a.com", "GET", "/v1/").unwrap();
    let p = Policy::new(vec![r]);
    assert!(p
        .decide(&req("a.com", 443, "GET", "/v1/x", "https"))
        .is_allowed());
    assert!(!p
        .decide(&req("a.com", 443, "GET", "/v1", "https"))
        .is_allowed());
    assert!(!p
        .decide(&req("a.com", 443, "GET", "/v1x", "https"))
        .is_allowed());
}

// Credential injection

fn binding() -> CredentialBinding {
    CredentialBinding::new(
        "api.anthropic.com",
        &["POST"],
        Some("/v1/messages"),
        "x-api-key",
        HeaderFormat::Raw,
        "ANTHROPIC_API_KEY",
    )
    .unwrap()
}

fn injected(plan: &InjectionPlan) -> Option<&HeaderInjection> {
    match &plan.outcome {
        InjectionOutcome::Inject(i) => Some(i),
        _ => None,
    }
}

#[test]
fn injects_for_exact_bound_host_over_https_on_allowed_method() {
    for host in [
        "api.anthropic.com",
        "API.ANTHROPIC.COM",
        "api.anthropic.com.",
    ] {
        let plan = plan_injection(
            &req(host, 443, "POST", "/v1/messages", "https"),
            &[binding()],
        );
        let i = injected(&plan).expect("should inject");
        assert_eq!(i.header, "x-api-key");
        assert_eq!(i.secret_name, "ANTHROPIC_API_KEY");
        assert_eq!(i.format, HeaderFormat::Raw);
        assert!(plan.should_strip("Authorization"));
        assert!(plan.should_strip("X-API-KEY"));
        assert!(plan.should_strip("x_api_key"));
        assert!(!plan.should_strip("content-type"));
    }
}

#[test]
fn refuses_injection_but_still_strips_on_bound_host() {
    let cases = [
        (
            req("api.anthropic.com", 443, "POST", "/v1/messages", "http"),
            RefusalReason::NotHttps,
        ),
        (
            req("api.anthropic.com", 8443, "POST", "/v1/messages", "https"),
            RefusalReason::PortNotBound,
        ),
        (
            req("api.anthropic.com", 443, "GET", "/v1/messages", "https"),
            RefusalReason::MethodNotBound,
        ),
        (
            req("api.anthropic.com", 443, "POST", "/v1/files", "https"),
            RefusalReason::PathNotBound,
        ),
        (
            req(
                "api.anthropic.com",
                443,
                "POST",
                "/v1/messages/../files",
                "https",
            ),
            RefusalReason::PathNotBound,
        ),
    ];
    for (request, reason) in cases {
        let plan = plan_injection(&request, &[binding()]);
        assert_eq!(
            plan.outcome,
            InjectionOutcome::Refused(reason),
            "{request:?}"
        );
        // An attacker's own key must not pass through either.
        assert!(plan.should_strip("authorization"));
        assert!(plan.should_strip("x-api-key"));
    }
}

#[test]
fn no_injection_for_other_hosts() {
    for host in [
        "evil.com",
        "api.anthropic.com.evil.com",
        "evil-api.anthropic.com",
        "sub.api.anthropic.com",
        "anthropic.com",
        "api.anthropic.com@evil.com",
        "127.0.0.1",
        "",
    ] {
        let plan = plan_injection(
            &req(host, 443, "POST", "/v1/messages", "https"),
            &[binding()],
        );
        assert_eq!(plan.outcome, InjectionOutcome::NotBound, "{host}");
        assert!(plan.strip_headers.is_empty());
    }
}

#[test]
fn bindings_refuse_wildcards_and_ips() {
    let make = |host: &str| {
        CredentialBinding::new(host, &["POST"], None, "x-api-key", HeaderFormat::Raw, "K")
    };
    assert_eq!(make("*.anthropic.com"), Err(BindingError::NotAnExactName));
    assert_eq!(make("8.8.8.8"), Err(BindingError::NotAnExactName));
    assert_eq!(make("0x7f.1"), Err(BindingError::NotAnExactName));
    assert_eq!(make("[::1]"), Err(BindingError::NotAnExactName));
    assert_eq!(make("localhost"), Err(BindingError::ReservedName));
    assert!(matches!(make("a b.com"), Err(BindingError::InvalidHost(_))));
    assert!(make("api.example.com").is_ok());

    let b = |methods: &[&str], path, header, name| {
        CredentialBinding::new("a.com", methods, path, header, HeaderFormat::Bearer, name)
    };
    assert_eq!(b(&[], None, "h", "K"), Err(BindingError::InvalidMethods));
    assert_eq!(
        b(&["get"], None, "h", "K"),
        Err(BindingError::InvalidMethods)
    );
    assert_eq!(
        b(&["GET"], Some("/a/.."), "h", "K"),
        Err(BindingError::InvalidPathPrefix)
    );
    assert_eq!(
        b(&["GET"], None, "X-Key", "K"),
        Err(BindingError::InvalidHeader)
    );
    assert_eq!(
        b(&["GET"], None, "h: x\r\n", "K"),
        Err(BindingError::InvalidHeader)
    );
    assert_eq!(
        b(&["GET"], None, "h", "sk-value"),
        Err(BindingError::InvalidSecretName)
    );
}

#[test]
fn redirect_targets_never_get_a_credential() {
    let target = req("api.anthropic.com", 443, "POST", "/v1/messages", "https");
    let plan = plan_redirect(&target, &[binding()]);
    assert_eq!(
        plan.outcome,
        InjectionOutcome::Refused(RefusalReason::RedirectTarget)
    );
    assert!(plan.should_strip("x-api-key"));
    let other = plan_redirect(&get("evil.com"), &[binding()]);
    assert_eq!(other.outcome, InjectionOutcome::NotBound);
}

#[test]
fn custom_header_is_added_to_strip_list_once() {
    let b = CredentialBinding::new(
        "a.com",
        &["GET"],
        None,
        "x-custom-token",
        HeaderFormat::Bearer,
        "K",
    )
    .unwrap();
    let plan = plan_injection(
        &req("a.com", 443, "GET", "/", "https"),
        std::slice::from_ref(&b),
    );
    assert!(plan.should_strip("X-Custom-Token"));
    assert_eq!(plan.strip_headers.len(), ALWAYS_STRIPPED_HEADERS.len() + 1);
    assert_eq!(injected(&plan).unwrap().format, HeaderFormat::Bearer);
    assert_eq!(b.host(), "a.com");
    assert_eq!(b.secret_name(), "K");
}

#[test]
fn multiple_bindings_for_one_host_use_the_first_that_accepts() {
    let a = CredentialBinding::new(
        "a.com",
        &["GET"],
        Some("/a"),
        "x-api-key",
        HeaderFormat::Raw,
        "A",
    )
    .unwrap();
    let b = CredentialBinding::new(
        "a.com",
        &["GET"],
        Some("/b"),
        "x-api-key",
        HeaderFormat::Raw,
        "B",
    )
    .unwrap();
    let plan = plan_injection(&req("a.com", 443, "GET", "/b/x", "https"), &[a, b]);
    assert_eq!(injected(&plan).unwrap().secret_name, "B");
}

// Blocked events

#[test]
fn blocked_event_drops_query_fragment_and_userinfo() {
    let r = req(
        "user:hunter2@evil.com/x?y=hunter2",
        443,
        "PUT",
        "/upload?token=hunter2&a=1#frag-hunter2",
        "https",
    );
    let e = BlockedEvent::new(&r, &DenyReason::NoMatchingRule);
    assert_eq!(e.host, "evil.com");
    assert_eq!(e.path, "/upload");
    let rendered = format!("{e} {e:?} {}", yaml_of(&e));
    assert!(!rendered.contains("hunter2"), "{rendered}");
    assert!(!rendered.contains("frag"));
    assert_eq!(
        e.to_string(),
        "blocked PUT https://evil.com:443/upload (no rule matches this host)"
    );
}

fn yaml_of(e: &BlockedEvent) -> String {
    serde_norway::to_string(e).unwrap()
}

#[test]
fn rule_labels_cannot_carry_control_characters() {
    for bad in ["", "a\nb", "a\u{1b}[2J", "é", &"x".repeat(129)] {
        let spec = RuleSpec {
            name: Some(bad.to_owned()),
            host: "a.com".to_owned(),
            ports: vec![443],
            methods: vec!["GET".to_owned()],
            schemes: None,
            path_prefix: None,
        };
        assert_eq!(
            Rule::new(spec),
            Err(PolicyError::InvalidRuleName),
            "{bad:?}"
        );
    }
}

#[test]
fn blocked_event_escapes_the_reason_too() {
    let reason = DenyReason::MethodNotAllowed {
        rule: "forged\nline".to_owned(),
    };
    let e = BlockedEvent::new(&get("a.com"), &reason);
    assert!(!e.reason.contains('\n'));
}

#[test]
fn blocked_event_escapes_control_and_non_ascii_characters() {
    let r = req(
        "evil\u{1b}[31m.com",
        443,
        "GE\nT",
        "/a\r\nFAKE LOG LINE\u{7f}\u{202e}\\x",
        "ht\ttps",
    );
    let e = BlockedEvent::new(&r, &DenyReason::InvalidMethod);
    let line = e.to_string();
    assert!(line.chars().all(|c| (' '..='~').contains(&c)), "{line:?}");
    assert!(e.host.contains("\\u{1b}"));
    assert!(e.method.contains("\\u{a}"));
    assert!(e
        .path
        .contains("\\u{d}\\u{a}FAKE LOG LINE\\u{7f}\\u{202e}\\\\x"));
    assert_eq!(e.scheme, "ht\\u{9}tps");
}

#[test]
fn blocked_event_bounds_length_and_refuses_absolute_targets() {
    let long = format!("/{}", "a".repeat(5000));
    let e = BlockedEvent::new(
        &req("evil.com", 443, "GET", &long, "https"),
        &DenyReason::NoMatchingRule,
    );
    assert_eq!(e.path.len(), 1024 + 3);

    let e = BlockedEvent::new(
        &req(
            "evil.com",
            443,
            "GET",
            "http://user:hunter2@evil.com/",
            "https",
        ),
        &DenyReason::InvalidPath,
    );
    assert!(!e.to_string().contains("hunter2"));
}

#[test]
fn request_debug_hides_query_strings() {
    let r = req("user:pw@a.com", 443, "GET", "/x?token=hunter2", "https");
    let text = format!("{r:?}");
    assert!(
        !text.contains("hunter2") && !text.contains("user:pw"),
        "{text}"
    );
}

// Policy files

const POLICY_YAML: &str = r#"
version: 1
rules:
  - name: npm
    host: registry.npmjs.org
    ports: [443]
    methods: [GET, HEAD]
  - host: "*.example.com"
    ports: [443, 8443]
    methods: [GET]
    schemes: [https, http]
    path_prefix: /public
"#;

#[test]
fn policy_file_parses_and_round_trips() {
    let p = Policy::from_yaml(POLICY_YAML).unwrap();
    assert_eq!(p.rules().len(), 2);
    assert_eq!(p.rules()[0].label(), "npm");
    assert_eq!(p.rules()[1].label(), "*.example.com");
    assert!(p.decide(&get("registry.npmjs.org")).is_allowed());
    assert!(p
        .decide(&req("a.example.com", 8443, "GET", "/public/x", "http"))
        .is_allowed());
    let again = Policy::from_yaml(&p.to_yaml().unwrap()).unwrap();
    assert_eq!(again, p);
}

#[test]
fn policy_file_rejects_unknown_keys_and_bad_values() {
    let rule_yaml = |extra: &str| {
        format!(
            "version: 1\nrules:\n  - host: a.com\n    ports: [443]\n    methods: [GET]\n{extra}"
        )
    };
    for (label, text) in [
        (
            "top-level key",
            "version: 1\nrules: []\nallow_all: true\n".to_owned(),
        ),
        ("rule key", rule_yaml("    allow_all: true\n")),
        ("typo", rule_yaml("    path_prefx: /x\n")),
        ("duplicate key", rule_yaml("    ports: [80]\n")),
    ] {
        assert!(
            matches!(Policy::from_yaml(&text), Err(PolicyError::Parse(_))),
            "{label}"
        );
    }
    assert_eq!(
        Policy::from_yaml("version: 2\nrules: []\n"),
        Err(PolicyError::UnsupportedVersion(2))
    );
    assert!(matches!(
        Policy::from_yaml("rules: []\n"),
        Err(PolicyError::Parse(_))
    ));
    assert!(matches!(
        Policy::from_yaml("- not\n- a\n- map\n"),
        Err(PolicyError::Parse(_))
    ));
    assert!(matches!(Policy::from_yaml(""), Err(PolicyError::Parse(_))));

    let bad_rule = |host: &str, ports: &str, methods: &str| {
        format!(
            "version: 1\nrules:\n  - host: {host}\n    ports: {ports}\n    methods: {methods}\n"
        )
    };
    assert_eq!(
        Policy::from_yaml(&bad_rule("\"*\"", "[443]", "[GET]")),
        Err(PolicyError::InvalidWildcard)
    );
    assert_eq!(
        Policy::from_yaml(&bad_rule("a.com", "[]", "[GET]")),
        Err(PolicyError::NoPorts)
    );
    assert_eq!(
        Policy::from_yaml(&bad_rule("a.com", "[0]", "[GET]")),
        Err(PolicyError::InvalidPort)
    );
    assert_eq!(
        Policy::from_yaml(&bad_rule("a.com", "[443]", "[]")),
        Err(PolicyError::NoMethods)
    );
    assert_eq!(
        Policy::from_yaml(&bad_rule("a.com", "[443]", "[get]")),
        Err(PolicyError::InvalidMethod)
    );
    assert_eq!(
        Policy::from_yaml(&bad_rule("a.com", "[443]", "[CONNECT]")),
        Err(PolicyError::ForbiddenMethod)
    );
    assert_eq!(
        Policy::from_yaml(&bad_rule("169.254.169.254", "[443]", "[GET]")),
        Err(PolicyError::BlockedAddress(IpClass::Metadata))
    );
    assert!(matches!(
        Policy::from_yaml(&bad_rule("a.com", "[70000]", "[GET]")),
        Err(PolicyError::Parse(_))
    ));
    assert!(matches!(
        Policy::from_yaml(&format!(
            "{}    schemes: [ftp]\n",
            bad_rule("a.com", "[443]", "[GET]")
        )),
        Err(PolicyError::Parse(_))
    ));
    assert_eq!(
        Policy::from_yaml(&format!(
            "{}    schemes: []\n",
            bad_rule("a.com", "[443]", "[GET]")
        )),
        Err(PolicyError::NoSchemes)
    );
    assert_eq!(
        Policy::from_yaml(&format!(
            "{}    path_prefix: /a/../b\n",
            bad_rule("a.com", "[443]", "[GET]")
        )),
        Err(PolicyError::InvalidPathPrefix)
    );
}

#[test]
fn policy_file_size_is_limited() {
    let big = format!("version: 1\n#{}\n", "x".repeat(MAX_POLICY_FILE_BYTES));
    assert!(matches!(
        Policy::from_yaml(&big),
        Err(PolicyError::TooLarge { .. })
    ));
}

#[test]
fn unquoted_numeric_ip_host_in_yaml_is_refused_not_coerced() {
    let text = "version: 1\nrules:\n  - host: 2130706433\n    ports: [443]\n    methods: [GET]\n";
    assert!(Policy::from_yaml(text).is_err());
}

// Policy change classification

type Spec<'a> = (&'a str, &'a [u16], &'a [&'a str], Option<&'a str>);

fn policy(specs: &[Spec]) -> Policy {
    Policy::new(
        specs
            .iter()
            .map(|(host, ports, methods, prefix)| {
                Rule::new(RuleSpec {
                    name: None,
                    host: (*host).to_owned(),
                    ports: ports.to_vec(),
                    methods: methods.iter().map(|m| (*m).to_owned()).collect(),
                    schemes: None,
                    path_prefix: prefix.map(str::to_owned),
                })
                .unwrap()
            })
            .collect(),
    )
}

#[test]
fn identical_reordered_and_relabelled_policies_are_unchanged() {
    let a = policy(&[
        ("a.com", &[443], &["GET", "HEAD"], None),
        ("b.com", &[443], &["GET"], None),
    ]);
    let b = policy(&[
        ("b.com", &[443], &["GET"], None),
        ("a.com", &[443], &["HEAD", "GET"], None),
    ]);
    assert_eq!(classify_change(&a, &a).kind, ChangeKind::Unchanged);
    let change = classify_change(&a, &b);
    assert_eq!(change.kind, ChangeKind::Unchanged);
    assert!(!change.requires_approval());
}

#[test]
fn tightening_changes_need_no_approval() {
    let old = policy(&[
        ("a.com", &[443, 8443], &["GET", "PUT"], None),
        ("b.com", &[443], &["GET"], None),
    ]);
    for new in [
        policy(&[("a.com", &[443, 8443], &["GET", "PUT"], None)]),
        policy(&[
            ("a.com", &[443], &["GET", "PUT"], None),
            ("b.com", &[443], &["GET"], None),
        ]),
        policy(&[
            ("a.com", &[443, 8443], &["GET"], None),
            ("b.com", &[443], &["GET"], None),
        ]),
        policy(&[
            ("a.com", &[443, 8443], &["GET", "PUT"], Some("/v1")),
            ("b.com", &[443], &["GET"], None),
        ]),
        Policy::deny_all(),
    ] {
        let change = classify_change(&old, &new);
        assert_eq!(change.kind, ChangeKind::Tightening, "{new:?}");
        assert!(!change.requires_approval());
        assert!(change.added.is_empty());
        assert!(!change.removed.is_empty());
    }
}

#[test]
fn narrowing_a_wildcard_or_prefix_is_tightening() {
    let wide = policy(&[("*.a.com", &[443], &["GET"], Some("/v1"))]);
    for narrow in [
        policy(&[("x.a.com", &[443], &["GET"], Some("/v1"))]),
        policy(&[("*.x.a.com", &[443], &["GET"], Some("/v1"))]),
        policy(&[("*.a.com", &[443], &["GET"], Some("/v1/deep"))]),
    ] {
        assert_eq!(classify_change(&wide, &narrow).kind, ChangeKind::Tightening);
        assert_eq!(classify_change(&narrow, &wide).kind, ChangeKind::Loosening);
    }
}

#[test]
fn loosening_changes_need_approval() {
    let old = policy(&[("a.com", &[443], &["GET"], Some("/v1"))]);
    for new in [
        policy(&[
            ("a.com", &[443], &["GET"], Some("/v1")),
            ("b.com", &[443], &["GET"], None),
        ]),
        policy(&[("a.com", &[443, 80], &["GET"], Some("/v1"))]),
        policy(&[("a.com", &[443], &["GET", "PUT"], Some("/v1"))]),
        policy(&[("a.com", &[443], &["GET"], None)]),
        policy(&[("a.com", &[443], &["GET"], Some("/v"))]),
        policy(&[("*.a.com", &[443], &["GET"], Some("/v1"))]),
        policy(&[("a.com", &[443], &["GET"], Some("/v2"))]),
    ] {
        let change = classify_change(&old, &new);
        assert_eq!(change.kind, ChangeKind::Loosening, "{new:?}");
        assert!(change.requires_approval());
        assert!(!change.added.is_empty());
    }
}

#[test]
fn adding_http_scheme_is_loosening() {
    let https = Policy::from_yaml(
        "version: 1\nrules:\n  - host: a.com\n    ports: [443]\n    methods: [GET]\n",
    )
    .unwrap();
    let both = Policy::from_yaml(
        "version: 1\nrules:\n  - host: a.com\n    ports: [443]\n    methods: [GET]\n    schemes: [https, http]\n",
    )
    .unwrap();
    let change = classify_change(&https, &both);
    assert_eq!(change.kind, ChangeKind::Loosening);
    assert_eq!(change.added, vec!["GET http://a.com:443".to_owned()]);
    assert_eq!(classify_change(&both, &https).kind, ChangeKind::Tightening);
}

#[test]
fn a_change_that_adds_and_removes_is_loosening() {
    let old = policy(&[("a.com", &[443], &["GET"], None)]);
    let new = policy(&[("b.com", &[443], &["GET"], None)]);
    let change = classify_change(&old, &new);
    assert_eq!(change.kind, ChangeKind::Loosening);
    assert_eq!(change.added, vec!["GET https://b.com:443".to_owned()]);
    assert_eq!(change.removed, vec!["GET https://a.com:443".to_owned()]);
}

#[test]
fn wildcard_does_not_cover_its_own_apex() {
    let old = policy(&[("*.a.com", &[443], &["GET"], None)]);
    let new = policy(&[("a.com", &[443], &["GET"], None)]);
    assert_eq!(classify_change(&old, &new).kind, ChangeKind::Loosening);
}
