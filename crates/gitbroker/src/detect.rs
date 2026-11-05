//! A small hand-written detector for likely secrets in added lines.
//!
//! It returns only which rule matched. The matched value is never stored or
//! returned. This catches careless leaks: a deliberate exfiltration can defeat
//! it with base64, string concatenation or UTF-16.

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretRule {
    AwsAccessKey,
    AwsSecretKey,
    GithubToken,
    PrivateKey,
    AnthropicKey,
    NpmToken,
    /// A long random-looking value given to a secret-sounding name. Only the
    /// snapshot scan raises this one.
    HighEntropy,
}

fn is_alnum(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

/// All offsets where `needle` starts and the previous byte is not alphanumeric.
fn starts<'a>(line: &'a [u8], needle: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
    line.windows(needle.len())
        .enumerate()
        .filter(move |(i, w)| *w == needle && (*i == 0 || !is_alnum(line[*i - 1])))
        .map(|(i, _)| i)
}

fn run_len(bytes: &[u8], allowed: impl Fn(u8) -> bool) -> usize {
    bytes.iter().take_while(|b| allowed(**b)).count()
}

fn has_prefixed(
    line: &[u8],
    prefix: &[u8],
    min: usize,
    allowed: impl Fn(u8) -> bool + Copy,
) -> bool {
    starts(line, prefix).any(|at| run_len(&line[at + prefix.len()..], allowed) >= min)
}

fn aws_access_key(line: &[u8]) -> bool {
    let body = |b: u8| b.is_ascii_uppercase() || b.is_ascii_digit();
    [b"AKIA", b"ASIA"]
        .iter()
        .any(|prefix| starts(line, *prefix).any(|at| run_len(&line[at + 4..], body) == 16))
}

fn aws_secret_key(line: &[u8]) -> bool {
    let lower = line.to_ascii_lowercase();
    let found = starts(&lower, b"aws_secret_access_key").any(|at| {
        let rest = &line[at + b"aws_secret_access_key".len()..];
        let skip = run_len(rest, |b| {
            matches!(b, b' ' | b'\t' | b'=' | b':' | b'"' | b'\'')
        });
        let body = |b: u8| is_alnum(b) || matches!(b, b'/' | b'+' | b'=');
        run_len(&rest[skip..], body) >= 40
    });
    found
}

fn github_token(line: &[u8]) -> bool {
    [b"ghp_", b"gho_", b"ghu_", b"ghs_", b"ghr_"]
        .iter()
        .any(|p| has_prefixed(line, *p, 36, is_alnum))
        || has_prefixed(line, b"github_pat_", 22, |b| is_alnum(b) || b == b'_')
}

fn private_key(line: &[u8]) -> bool {
    starts(line, b"-----BEGIN ").any(|at| {
        line[at..]
            .windows(b"PRIVATE KEY".len())
            .any(|w| w == b"PRIVATE KEY")
    })
}

fn anthropic_key(line: &[u8]) -> bool {
    has_prefixed(line, b"sk-ant-", 16, |b| {
        is_alnum(b) || b == b'_' || b == b'-'
    })
}

fn npm_token(line: &[u8]) -> bool {
    has_prefixed(line, b"npm_", 36, is_alnum)
        || line
            .windows(b"_authToken=".len())
            .enumerate()
            .filter(|(_, w)| *w == b"_authToken=")
            .any(|(i, _)| {
                let value = &line[i + b"_authToken=".len()..];
                // `${NPM_TOKEN}` is a reference to a variable, not a value.
                run_len(value, is_alnum_or_token_char) >= 8
            })
}

fn is_alnum_or_token_char(b: u8) -> bool {
    is_alnum(b) || matches!(b, b'-' | b'_' | b'.' | b'/' | b'+' | b'=')
}

/// Characters a token is made of. A value ends at anything else, so `${VAR}`,
/// `<your key here>` and a URL all stop long before the length threshold.
fn is_value_char(b: u8) -> bool {
    is_alnum(b) || matches!(b, b'+' | b'/' | b'=' | b'_' | b'-' | b'.')
}

/// A name holding one of these is worth looking at the value of.
const SENSITIVE_WORDS: &[&str] = &[
    "key",
    "token",
    "secret",
    "password",
    "passwd",
    "pwd",
    "credential",
    "auth",
    "access",
    "private",
    "session",
    "signature",
    "salt",
];

/// Values that are meant to be replaced. They can still look random.
const PLACEHOLDERS: &[&str] = &[
    "example",
    "sample",
    "changeme",
    "change-me",
    "change_me",
    "your",
    "placeholder",
    "dummy",
    "redacted",
    "replace",
    "todo",
    "xxxx",
];

/// Shortest value considered, and the bits per character it must reach. Both are
/// set so that ordinary identifiers and sentences stay below them.
const MIN_VALUE_LEN: usize = 20;
const MIN_ENTROPY: f64 = 3.7;
const MAX_VALUE_LEN: usize = 4096;

/// Shannon entropy of the bytes, in bits per character.
fn entropy_bits(value: &[u8]) -> f64 {
    let mut counts = [0u32; 256];
    for byte in value {
        counts[*byte as usize] += 1;
    }
    let len = value.len() as f64;
    counts
        .iter()
        .filter(|count| **count > 0)
        .map(|count| {
            let share = f64::from(*count) / len;
            -share * share.log2()
        })
        .sum()
}

/// The identifier right before a separator, lowercased. Spaces and quotes
/// between the name and the separator are skipped, so `"apiKey" :` is read the
/// same as `apikey=`.
fn name_before(prefix: &[u8]) -> String {
    let is_name_char = |b: u8| is_alnum(b) || matches!(b, b'_' | b'-' | b'.');
    let end = prefix.len() - run_len_back(prefix, |b| matches!(b, b' ' | b'\t' | b'"' | b'\''));
    let head = &prefix[..end];
    let start = head
        .iter()
        .rposition(|b| !is_name_char(*b))
        .map(|at| at + 1)
        .unwrap_or(0);
    String::from_utf8_lossy(&head[start..]).to_ascii_lowercase()
}

/// How many trailing bytes satisfy `allowed`.
fn run_len_back(bytes: &[u8], allowed: impl Fn(u8) -> bool) -> usize {
    bytes.iter().rev().take_while(|b| allowed(**b)).count()
}

fn looks_random(value: &[u8]) -> bool {
    if value.len() < MIN_VALUE_LEN || value.len() > MAX_VALUE_LEN {
        return false;
    }
    let lower = String::from_utf8_lossy(value).to_ascii_lowercase();
    if PLACEHOLDERS.iter().any(|word| lower.contains(word)) {
        return false;
    }
    let has_digit = value.iter().any(u8::is_ascii_digit);
    let has_letter = value.iter().any(u8::is_ascii_alphabetic);
    has_digit && has_letter && entropy_bits(value) >= MIN_ENTROPY
}

/// True when the line gives a secret-sounding name a long random-looking value.
///
/// This is a warning signal, not proof, and it is deliberately narrow: without
/// the name, every checksum and minified bundle would match. It never keeps or
/// returns the value.
pub(crate) fn high_entropy_value(line: &[u8]) -> bool {
    for (at, byte) in line.iter().enumerate() {
        if !matches!(*byte, b'=' | b':') {
            continue;
        }
        let name = name_before(&line[..at]);
        if name.is_empty() || !SENSITIVE_WORDS.iter().any(|word| name.contains(word)) {
            continue;
        }
        let rest = &line[at + 1..];
        let skip = run_len(rest, |b| matches!(b, b' ' | b'\t' | b'"' | b'\'' | b'>'));
        let value = &rest[skip..];
        let end = run_len(value, is_value_char);
        if looks_random(&value[..end]) {
            return true;
        }
    }
    false
}

type Check = (SecretRule, fn(&[u8]) -> bool);

/// Which rules match one line. Works on bytes, so it also runs over binaries.
pub(crate) fn scan_line(line: &[u8]) -> Vec<SecretRule> {
    let checks: [Check; 6] = [
        (SecretRule::AwsAccessKey, aws_access_key),
        (SecretRule::AwsSecretKey, aws_secret_key),
        (SecretRule::GithubToken, github_token),
        (SecretRule::PrivateKey, private_key),
        (SecretRule::AnthropicKey, anthropic_key),
        (SecretRule::NpmToken, npm_token),
    ];
    checks
        .iter()
        .filter(|(_, check)| check(line))
        .map(|(rule, _)| *rule)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(line: &str) -> Vec<SecretRule> {
        scan_line(line.as_bytes())
    }

    #[test]
    fn detects_each_rule() {
        let aws = format!("key = \"AKIA{}\"", "IOSFODNN7EXAMPLE");
        assert_eq!(rules(&aws), [SecretRule::AwsAccessKey]);
        let sts = format!("ASIA{}", "A".repeat(16));
        assert_eq!(rules(&sts), [SecretRule::AwsAccessKey]);
        let secret = format!("aws_secret_access_key = {}", "w".repeat(40));
        assert_eq!(rules(&secret), [SecretRule::AwsSecretKey]);
        let secret_upper = format!("AWS_SECRET_ACCESS_KEY=\"{}\"", "w".repeat(40));
        assert_eq!(rules(&secret_upper), [SecretRule::AwsSecretKey]);
        for prefix in ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"] {
            let token = format!("{prefix}{}", "a".repeat(36));
            assert_eq!(rules(&token), [SecretRule::GithubToken], "{prefix}");
        }
        let pat = format!("github_pat_{}", "a_".repeat(11));
        assert_eq!(rules(&pat), [SecretRule::GithubToken]);
        for header in [
            "-----BEGIN RSA PRIVATE KEY-----",
            "-----BEGIN PRIVATE KEY-----",
            "-----BEGIN OPENSSH PRIVATE KEY-----",
            "-----BEGIN PGP PRIVATE KEY BLOCK-----",
        ] {
            assert_eq!(rules(header), [SecretRule::PrivateKey], "{header}");
        }
        let anthropic = format!("sk-ant-{}", "api03-abcdefghijkl");
        assert_eq!(rules(&anthropic), [SecretRule::AnthropicKey]);
        let npm = format!("npm_{}", "a".repeat(36));
        assert_eq!(rules(&npm), [SecretRule::NpmToken]);
        let npmrc = format!("//registry.npmjs.org/:_authToken={}", "abc123def456");
        assert_eq!(rules(&npmrc), [SecretRule::NpmToken]);
    }

    #[test]
    fn avoids_common_false_positives() {
        for line in [
            "",
            "let x = 1;",
            "AKIA",
            "AKIASHORT",
            "ghp_short",
            "-----BEGIN PUBLIC KEY-----",
            "-----BEGIN CERTIFICATE-----",
            "sk-ant-",
            "npm_config_registry=x",
            "//registry.npmjs.org/:_authToken=${NPM_TOKEN}",
            "aws_secret_access_key = ${AWS_SECRET}",
        ] {
            assert!(rules(line).is_empty(), "{line}");
        }
        let embedded = format!("XAKIA{}", "IOSFODNN7EXAMPLE");
        assert!(rules(&embedded).is_empty());
        let too_long = format!("AKIA{}", "A".repeat(30));
        assert!(rules(&too_long).is_empty());
    }

    #[test]
    fn high_entropy_values_need_a_secret_sounding_name() {
        for line in [
            "api_key = \"Xq7PzR2mWv9TkLbN4sYc8H\"",
            "DATABASE_PASSWORD=5f4dcc3b5aa765d61d8327deb882cf99",
            "{\"accessToken\": \"vTq8Zx3Lp0Nd6Hb2Rk9Jm4Wc7Yf1Gs5A\"}",
            "private-key: MIIBVgIBADANBgkqhkiG9w0BAQEFAASCAUAwggE8",
        ] {
            assert!(high_entropy_value(line.as_bytes()), "{line}");
        }
    }

    #[test]
    fn high_entropy_leaves_ordinary_lines_alone() {
        for line in [
            "",
            "let total = 1 + 2;",
            "name = my-application-service",
            "api_key = ${API_KEY}",
            "api_key = \"<your key here>\"",
            "token: changeme",
            "password = short",
            "integrity: sha512-Xq7PzR2mWv9TkLbN4sYc8HzzQw0eRt5Yu8Io9Pa2Sd4Fg6Hj",
            "url = https://user@example.invalid/a/very/long/path/to/a/thing",
            "secret_key = 00000000000000000000000000000000",
            "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMIK7MDENGbPxRfiCYEXAMPLEKEY1",
        ] {
            assert!(!high_entropy_value(line.as_bytes()), "{line}");
        }
    }

    #[test]
    fn handles_arbitrary_bytes() {
        let mut line = vec![0xff, 0x00];
        line.extend_from_slice(format!("AKIA{}", "IOSFODNN7EXAMPLE").as_bytes());
        line.push(0xfe);
        assert_eq!(scan_line(&line), [SecretRule::AwsAccessKey]);
    }
}
