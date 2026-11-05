use std::fs;

use secrets::*;

const SENTINEL: &str = "hunter2-sentinel-value";

fn name(s: &str) -> SecretName {
    SecretName::new(s).unwrap()
}

fn service(s: &str) -> ServiceName {
    ServiceName::new(s).unwrap()
}

fn values(text: &str) -> Vec<(String, String)> {
    parse_env(text)
        .unwrap()
        .into_iter()
        .map(|e| (e.name.to_string(), e.value.expose().to_owned()))
        .collect()
}

fn one(text: &str) -> String {
    let parsed = values(text);
    assert_eq!(parsed.len(), 1, "expected one entry");
    parsed.into_iter().next().unwrap().1
}

// SecretValue

#[test]
fn secret_value_is_redacted_in_debug_and_display() {
    let v = SecretValue::new(SENTINEL);
    assert!(!format!("{v:?}").contains(SENTINEL));
    assert!(!format!("{v}").contains(SENTINEL));
    assert!(!format!("{:#?}", vec![&v]).contains(SENTINEL));
    assert_eq!(v.expose(), SENTINEL);
}

#[test]
fn storage_type_zeroes_on_drop() {
    fn assert_zeroize_on_drop<T: zeroize::ZeroizeOnDrop>() {}
    assert_zeroize_on_drop::<zeroize::Zeroizing<String>>();
}

#[test]
fn env_entry_debug_hides_value() {
    let entries = parse_env(&format!("A={SENTINEL}\n")).unwrap();
    assert!(!format!("{entries:?}").contains(SENTINEL));
}

// .env parser

#[test]
fn parses_comments_blanks_export_and_quotes() {
    let parsed = values(
        "# comment\n\n  export A=1\nB='two # not comment'\nC=\"three\"\nexport\tD = four # comment\nE=\nexportF=5\n",
    );
    let expected = [
        ("A", "1"),
        ("B", "two # not comment"),
        ("C", "three"),
        ("D", "four"),
        ("E", ""),
        ("exportF", "5"),
    ];
    assert_eq!(parsed.len(), expected.len());
    for ((n, v), (en, ev)) in parsed.iter().zip(expected) {
        assert_eq!((n.as_str(), v.as_str()), (en, ev));
    }
}

#[test]
fn hash_without_preceding_blank_is_part_of_the_value() {
    assert_eq!(one("A=abc#def\n"), "abc#def");
    assert_eq!(one("A=#fff\n"), "#fff");
    assert_eq!(one("A= # only comment\n"), "");
}

#[test]
fn double_quote_escapes() {
    assert_eq!(
        one(r#"A="l1\nl2\tt\\b\"q\'s\$x\`y\rz""#),
        "l1\nl2\tt\\b\"q's$x`y\rz"
    );
}

#[test]
fn single_quotes_are_fully_literal() {
    assert_eq!(one(r"A='a\nb$HOME'"), r"a\nb$HOME");
}

#[test]
fn unknown_escape_is_an_error_with_line_and_no_value() {
    let err = parse_env(&format!("OK=1\nA=\"{SENTINEL}\\q\"\n")).unwrap_err();
    assert_eq!(err, EnvError::BadEscape { line: 2 });
    assert_no_sentinel(&err);
}

#[test]
fn shell_syntax_stays_literal_and_runs_nothing() {
    assert_eq!(one("A=$(touch /tmp/x)\n"), "$(touch /tmp/x)");

    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("pwned");
    let m = marker.display();
    let text = format!(
        "A=$(touch {m})\nB=`touch {m}`\nC=\"$(touch {m})\"\nD='$(touch {m})'\nE=x; touch {m}\nF=${{HOME}}\nG=\"$HOME\"\nH=$HOME\n"
    );
    let parsed = values(&text);
    assert_eq!(parsed[0].1, format!("$(touch {m})"));
    assert_eq!(parsed[1].1, format!("`touch {m}`"));
    assert_eq!(parsed[2].1, format!("$(touch {m})"));
    assert_eq!(parsed[3].1, format!("$(touch {m})"));
    assert_eq!(parsed[4].1, format!("x; touch {m}"));
    assert_eq!(parsed[5].1, "${HOME}");
    assert_eq!(parsed[6].1, "$HOME");
    assert_eq!(parsed[7].1, "$HOME");
    assert!(!marker.exists(), "parsing must not run anything");
}

#[test]
fn crlf_and_bom_are_handled() {
    let parsed = values("\u{feff}A=1\r\nB=\"2\"\r\n# c\r\n\r\nC=3");
    assert_eq!(parsed[0], ("A".to_owned(), "1".to_owned()));
    assert_eq!(parsed[1], ("B".to_owned(), "2".to_owned()));
    assert_eq!(parsed[2], ("C".to_owned(), "3".to_owned()));
}

#[test]
fn quoted_values_can_span_lines_and_later_line_numbers_stay_correct() {
    let entries = parse_env("A=\"one\ntwo\"\nB='x\ny'\nC=1\nbad line\n");
    let err = entries.unwrap_err();
    assert_eq!(err, EnvError::MissingEquals { line: 6 });

    let entries = parse_env("A=\"one\ntwo\"\nB=2\n").unwrap();
    assert_eq!(entries[0].value.expose(), "one\ntwo");
    assert_eq!(entries[0].line, 1);
    assert_eq!(entries[1].line, 3);
}

#[test]
fn errors_report_lines_and_never_contain_values() {
    let cases: Vec<(String, EnvError)> = vec![
        (
            format!("OK=1\n{SENTINEL}\n"),
            EnvError::MissingEquals { line: 2 },
        ),
        (
            format!("OK=1\n\n{SENTINEL} =1\n").replace(SENTINEL, "sk-hunter2-sentinel-value"),
            EnvError::InvalidName { line: 3 },
        ),
        (
            format!("A=\"{SENTINEL}\n"),
            EnvError::UnterminatedQuote { line: 1 },
        ),
        (
            format!("A='{SENTINEL}\n"),
            EnvError::UnterminatedQuote { line: 1 },
        ),
        (
            format!("A=\"{SENTINEL}\"trailing\n"),
            EnvError::TrailingText { line: 1 },
        ),
        (
            format!("A='{SENTINEL}' x\n"),
            EnvError::TrailingText { line: 1 },
        ),
        (
            format!("A={SENTINEL}\nB=1\nA=2\n"),
            EnvError::DuplicateName {
                line: 3,
                first_line: 1,
            },
        ),
    ];
    for (text, expected) in cases {
        let err = parse_env(&text).unwrap_err();
        assert_eq!(err, expected);
        assert_no_sentinel(&err);
    }
}

fn assert_no_sentinel(err: &EnvError) {
    let rendered = format!("{err} {err:?}");
    assert!(!rendered.contains("hunter2"), "leak in {rendered}");
}

#[test]
fn invalid_names_are_rejected() {
    for bad in [
        "1A=x",
        "A-B=x",
        "A.B=x",
        "=x",
        "$A=x",
        "A$(x)=y",
        "é=x",
        "export =x",
    ] {
        let err = parse_env(bad).unwrap_err();
        assert_eq!(err, EnvError::InvalidName { line: 1 }, "input {bad}");
    }
    assert_eq!(
        parse_env("export A\n").unwrap_err(),
        EnvError::MissingEquals { line: 1 }
    );
    let long = format!("{}=x", "A".repeat(257));
    assert_eq!(
        parse_env(&long).unwrap_err(),
        EnvError::InvalidName { line: 1 }
    );
}

#[test]
fn nul_and_invalid_utf8_are_rejected_with_line() {
    assert_eq!(
        parse_env_bytes(b"A=1\nB=a\0b\n").unwrap_err(),
        EnvError::Nul { line: 2 }
    );
    assert_eq!(
        parse_env_bytes(b"A=1\nC=2\nB=\xff\n").unwrap_err(),
        EnvError::InvalidUtf8 { line: 3 }
    );
}

#[test]
fn size_limit_is_enforced() {
    let at_limit = format!("A={}", "x".repeat(MAX_ENV_FILE_BYTES - 2));
    assert_eq!(parse_env(&at_limit).unwrap().len(), 1);
    let over = format!("A={}", "x".repeat(MAX_ENV_FILE_BYTES - 1));
    assert_eq!(
        parse_env(&over).unwrap_err(),
        EnvError::TooLarge {
            limit: MAX_ENV_FILE_BYTES
        }
    );
}

#[test]
fn file_parsing_reads_regular_files_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.env");
    fs::write(&path, "A=1\n").unwrap();
    assert_eq!(parse_env_file(&path).unwrap()[0].value.expose(), "1");

    assert_eq!(
        parse_env_file(dir.path()).unwrap_err(),
        EnvError::NotRegularFile
    );

    let big = dir.path().join("big.env");
    fs::write(&big, vec![b'x'; MAX_ENV_FILE_BYTES + 1]).unwrap();
    assert!(matches!(
        parse_env_file(&big).unwrap_err(),
        EnvError::TooLarge { .. }
    ));

    assert!(matches!(
        parse_env_file(&dir.path().join("missing")).unwrap_err(),
        EnvError::Io { .. }
    ));
}

#[cfg(unix)]
#[test]
fn file_parsing_refuses_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    fs::write(&target, "A=1\n").unwrap();
    let link = dir.path().join("link.env");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert_eq!(parse_env_file(&link).unwrap_err(), EnvError::NotRegularFile);
}

// Stores

#[test]
fn memory_store_round_trip() {
    let store = MemoryStore::new();
    let n = name("KEY");
    assert!(store.get(&n).unwrap().is_none());
    store.put(&n, &SecretValue::new("one")).unwrap();
    store.put(&n, &SecretValue::new("two")).unwrap();
    assert_eq!(store.get(&n).unwrap().unwrap().expose(), "two");
    assert!(store.delete(&n).unwrap());
    assert!(!store.delete(&n).unwrap());
}

#[test]
fn crate_source_never_starts_a_process() {
    // Keeps values off command lines: no helper process exists to receive them.
    let needle_a = ["std::", "process"].concat();
    let needle_b = ["Command", "::new"].concat();
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
    for entry in fs::read_dir(src).unwrap() {
        let text = fs::read_to_string(entry.unwrap().path()).unwrap();
        assert!(!text.contains(&needle_a) && !text.contains(&needle_b));
    }
}

// Names

#[test]
fn name_validation() {
    assert!(SecretName::new("_A1").is_ok());
    for bad in ["", "1A", "A B", "A=B", "A/B", "Ä"] {
        assert_eq!(SecretName::new(bad).unwrap_err(), NameError::Secret);
    }
    assert!(ServiceName::new("web-1.a_b").is_ok());
    for bad in ["", "-a", "a b", "a/b", "../a"] {
        assert_eq!(ServiceName::new(bad).unwrap_err(), NameError::Service);
    }
    assert!(ProjectId::new("proj.1").is_ok());
    for bad in ["", "a/b", "a b", "/a"] {
        assert_eq!(ProjectId::new(bad).unwrap_err(), NameError::Project);
    }
}

#[test]
fn bound_host_must_be_one_exact_name() {
    assert!(BoundHost::new("api.anthropic.com").is_ok());
    for bad in [
        "",
        "localhost",
        "*.anthropic.com",
        "API.anthropic.com",
        "api.anthropic.com.",
        "127.0.0.1",
        "127.1",
        "0x7f.1",
        "2130706433",
        "[::1]",
        "user@api.anthropic.com",
        "api.anthropic.com:443",
        "api.anthropic.com/v1",
        "éxample.com",
        "a..b.com",
        "-a.com",
    ] {
        assert_eq!(
            BoundHost::new(bad).unwrap_err(),
            NameError::BoundHost,
            "input {bad}"
        );
    }
}

// Assignments

fn sample_set() -> AssignmentSet {
    AssignmentSet::new(vec![
        Assignment::new(
            name("ANTHROPIC_API_KEY"),
            Delivery::Proxy {
                bound_host: BoundHost::new("api.anthropic.com").unwrap(),
            },
            vec![service("agent")],
        )
        .unwrap(),
        Assignment::new(
            name("ENCRYPTION_KEY"),
            Delivery::Injected,
            vec![service("api"), service("worker")],
        )
        .unwrap(),
        Assignment::new(name("DB_PASSWORD"), Delivery::Injected, vec![service("db")]).unwrap(),
        Assignment::new(
            name("REGISTRY_TOKEN"),
            Delivery::BuildSecret,
            vec![service("web")],
        )
        .unwrap(),
    ])
    .unwrap()
}

fn sample_store() -> MemoryStore {
    let store = MemoryStore::new();
    for n in [
        "ANTHROPIC_API_KEY",
        "ENCRYPTION_KEY",
        "DB_PASSWORD",
        "REGISTRY_TOKEN",
    ] {
        store
            .put(&name(n), &SecretValue::new(format!("{SENTINEL}-{n}")))
            .unwrap();
    }
    store
}

/// A store that records which secrets were read.
struct RecordingStore {
    inner: MemoryStore,
    reads: std::sync::Mutex<Vec<String>>,
}

impl SecretStore for RecordingStore {
    fn put(&self, n: &SecretName, v: &SecretValue) -> Result<(), StoreError> {
        self.inner.put(n, v)
    }
    fn get(&self, n: &SecretName) -> Result<Option<SecretValue>, StoreError> {
        self.reads.lock().unwrap().push(n.to_string());
        self.inner.get(n)
    }
    fn delete(&self, n: &SecretName) -> Result<bool, StoreError> {
        self.inner.delete(n)
    }
}

#[test]
fn resolve_returns_only_what_the_service_may_get() {
    let set = sample_set();
    let store = sample_store();
    let placeholders = SandboxPlaceholders::generate(&set).unwrap();

    let api = set
        .resolve_for_service(&service("api"), &store, &placeholders)
        .unwrap();
    assert_eq!(api.injected.len(), 1);
    assert_eq!(api.injected[0].name, name("ENCRYPTION_KEY"));
    assert_eq!(
        api.injected[0].value.expose(),
        format!("{SENTINEL}-ENCRYPTION_KEY")
    );
    assert!(api.proxy.is_empty());
    assert!(api.build_secret_names.is_empty());

    let db = set
        .resolve_for_service(&service("db"), &store, &placeholders)
        .unwrap();
    assert_eq!(db.injected[0].name, name("DB_PASSWORD"));
    assert_eq!(db.injected.len(), 1);

    let unknown = set
        .resolve_for_service(&service("nobody"), &store, &placeholders)
        .unwrap();
    assert!(unknown.injected.is_empty() && unknown.proxy.is_empty());
}

#[test]
fn proxy_service_gets_a_placeholder_and_real_value_is_not_read() {
    let set = sample_set();
    let store = RecordingStore {
        inner: sample_store(),
        reads: Default::default(),
    };
    let placeholders = SandboxPlaceholders::generate(&set).unwrap();
    let agent = set
        .resolve_for_service(&service("agent"), &store, &placeholders)
        .unwrap();
    assert!(agent.injected.is_empty());
    assert_eq!(agent.proxy.len(), 1);
    assert_eq!(
        agent.proxy[0].bound_host,
        BoundHost::new("api.anthropic.com").unwrap()
    );
    assert!(!agent.proxy[0].placeholder.as_str().contains(SENTINEL));
    assert!(!format!("{agent:?}").contains(SENTINEL));
    assert!(store.reads.lock().unwrap().is_empty());
}

#[test]
fn build_secrets_are_not_in_start_resolution_and_need_opt_in() {
    let set = sample_set();
    let store = RecordingStore {
        inner: sample_store(),
        reads: Default::default(),
    };
    let placeholders = SandboxPlaceholders::generate(&set).unwrap();
    let web = set
        .resolve_for_service(&service("web"), &store, &placeholders)
        .unwrap();
    assert!(web.injected.is_empty());
    assert_eq!(web.build_secret_names, vec![name("REGISTRY_TOKEN")]);
    assert!(store.reads.lock().unwrap().is_empty());

    let none = set
        .resolve_build_secrets(&service("web"), &[], &store)
        .unwrap();
    assert!(none.is_empty());

    let got = set
        .resolve_build_secrets(&service("web"), &[name("REGISTRY_TOKEN")], &store)
        .unwrap();
    assert_eq!(got[0].value.expose(), format!("{SENTINEL}-REGISTRY_TOKEN"));

    for (svc, secret) in [
        ("api", "REGISTRY_TOKEN"),
        ("web", "ENCRYPTION_KEY"),
        ("agent", "ANTHROPIC_API_KEY"),
    ] {
        let err = set
            .resolve_build_secrets(&service(svc), &[name(secret)], &store)
            .unwrap_err();
        assert!(matches!(
            err,
            ResolveError::NotABuildSecretForService { .. }
        ));
    }
}

#[test]
fn missing_secret_and_missing_placeholder_fail_closed() {
    let set = sample_set();
    let empty = MemoryStore::new();
    let placeholders = SandboxPlaceholders::generate(&set).unwrap();
    assert!(matches!(
        set.resolve_for_service(&service("api"), &empty, &placeholders)
            .unwrap_err(),
        ResolveError::MissingSecret { .. }
    ));
    let no_placeholders = SandboxPlaceholders::generate(&AssignmentSet::default()).unwrap();
    assert!(no_placeholders.is_empty());
    assert!(matches!(
        set.resolve_for_service(&service("agent"), &empty, &no_placeholders)
            .unwrap_err(),
        ResolveError::MissingPlaceholder { .. }
    ));
}

#[test]
fn assignment_validation() {
    assert!(matches!(
        Assignment::new(name("A"), Delivery::Injected, vec![]).unwrap_err(),
        AssignmentError::NoServices { .. }
    ));
    assert!(matches!(
        Assignment::new(
            name("A"),
            Delivery::Injected,
            vec![service("x"), service("x")]
        )
        .unwrap_err(),
        AssignmentError::DuplicateService { .. }
    ));
    let a = Assignment::new(name("A"), Delivery::Injected, vec![service("x")]).unwrap();
    assert!(matches!(
        AssignmentSet::new(vec![a.clone(), a]).unwrap_err(),
        AssignmentError::DuplicateSecret { .. }
    ));
}

#[test]
fn metadata_round_trips_and_has_no_value_field() {
    let set = sample_set();
    let json = serde_json::to_string(&set).unwrap();
    assert!(!json.contains(SENTINEL));
    assert!(json.contains(r#""proxy":{"bound_host":"api.anthropic.com"}"#));
    assert!(json.contains(r#""delivery":"build_secret""#));
    let back: AssignmentSet = serde_json::from_str(&json).unwrap();
    assert_eq!(back, set);
}

#[test]
fn metadata_deserialization_validates_and_rejects_unknown_fields() {
    let leak = format!(
        r#"{{"assignments":[{{"name":"A","delivery":"injected","services":["x"],"value":"{SENTINEL}"}}]}}"#
    );
    let err = serde_json::from_str::<AssignmentSet>(&leak).unwrap_err();
    assert!(!err.to_string().contains(SENTINEL));

    for bad in [
        r#"{"assignments":[{"name":"A","delivery":"injected","services":[]}]}"#,
        r#"{"assignments":[{"name":"1A","delivery":"injected","services":["x"]}]}"#,
        r#"{"assignments":[{"name":"A","delivery":"mailed","services":["x"]}]}"#,
        r#"{"assignments":[{"name":"A","delivery":{"proxy":{"bound_host":"*.x.com"}},"services":["x"]}]}"#,
        r#"{"assignments":[{"name":"A","delivery":{"proxy":{"bound_host":"x.com","extra":1}},"services":["x"]}]}"#,
        r#"{"assignments":[{"name":"A","delivery":"injected","services":["x"]},{"name":"A","delivery":"injected","services":["y"]}]}"#,
        r#"{"assignments":[],"extra":1}"#,
    ] {
        assert!(serde_json::from_str::<AssignmentSet>(bad).is_err(), "{bad}");
    }
}

#[test]
fn proxy_bindings_list_only_proxy_secrets() {
    let bindings = sample_set().proxy_bindings();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].name, name("ANTHROPIC_API_KEY"));
    assert_eq!(bindings[0].bound_host.as_str(), "api.anthropic.com");
}

#[test]
fn resolve_errors_hold_names_not_values() {
    let set = sample_set();
    let store = MemoryStore::new();
    store
        .put(&name("OTHER"), &SecretValue::new(SENTINEL))
        .unwrap();
    let placeholders = SandboxPlaceholders::generate(&set).unwrap();
    let err = set
        .resolve_for_service(&service("api"), &store, &placeholders)
        .unwrap_err();
    assert!(!format!("{err} {err:?}").contains(SENTINEL));
    assert!(err.to_string().contains("ENCRYPTION_KEY"));
}

// Placeholders

#[test]
fn placeholders_are_random_labelled_and_per_sandbox() {
    let set = sample_set();
    let a = SandboxPlaceholders::generate(&set).unwrap();
    let b = SandboxPlaceholders::generate(&set).unwrap();
    assert_eq!(a.len(), 1);
    let n = name("ANTHROPIC_API_KEY");
    let pa = a.get(&n).unwrap().as_str();
    let pb = b.get(&n).unwrap().as_str();
    assert_ne!(pa, pb);
    let prefix = format!("{}-placeholder-", brand::PRODUCT_NAME);
    let hex = pa.strip_prefix(&prefix).expect("labelled as a placeholder");
    assert_eq!(hex.len(), 32);
    assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(a.get(&name("ENCRYPTION_KEY")).is_none());
}

// Audit

#[test]
fn audit_event_serializes_names_and_action_only() {
    let event = AuditEvent::new(
        42,
        AuditAction::InjectedAtStart,
        name("ENCRYPTION_KEY"),
        Some(service("api")),
    );
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "unix_seconds": 42,
            "action": "injected_at_start",
            "secret": "ENCRYPTION_KEY",
            "service": "api",
        })
    );
    let no_service = AuditEvent::new(1, AuditAction::Stored, name("A"), None);
    assert!(serde_json::to_value(&no_service)
        .unwrap()
        .get("service")
        .is_none());
}
