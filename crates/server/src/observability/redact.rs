//! Secret and PII redaction for logs, traces and diagnostic bundles.

use std::env;

/// Default secret canary injected by CI and local tests (`qncy_` prefix).
pub const DEFAULT_SECRET_CANARY: &str = "qncy_test_canary_not_for_prod";
/// Replacement for a canary value.
pub const CANARY_PLACEHOLDER: &str = "[REDACTED:canary]";

const TOKEN_PREFIXES: &[(&str, &str)] = &[
    ("Bearer ", "[REDACTED:bearer]"),
    ("sk-", "[REDACTED:secret]"),
    ("ghp_", "[REDACTED:secret]"),
    ("xoxb-", "[REDACTED:secret]"),
    ("AKIA", "[REDACTED:secret]"),
];

/// Canaries to scrub: `QUANSIO_TEST_SECRET_CANARY` plus the default test canary.
#[must_use]
pub fn secret_canaries() -> Vec<String> {
    let mut values = vec![DEFAULT_SECRET_CANARY.to_string()];
    if let Ok(extra) = env::var("QUANSIO_TEST_SECRET_CANARY") {
        if !extra.is_empty() && extra != DEFAULT_SECRET_CANARY {
            values.push(extra);
        }
    }
    values
}

/// Redact credential-shaped values, emails and secret canaries.
#[must_use]
pub fn redact_text(input: &str) -> String {
    let mut out = input.to_string();
    for canary in secret_canaries() {
        if !canary.is_empty() {
            out = out.replace(&canary, CANARY_PLACEHOLDER);
        }
    }
    for (prefix, placeholder) in TOKEN_PREFIXES {
        out = redact_token(&out, prefix, placeholder);
    }
    redact_emails(&out)
}

fn redact_token(input: &str, prefix: &str, placeholder: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(at) = rest.find(prefix) {
        out.push_str(&rest[..at]);
        out.push_str(placeholder);
        let after = &rest[at + prefix.len()..];
        let skip = after
            .find(|ch: char| ch.is_whitespace() || ch == '"' || ch == '\'')
            .unwrap_or(after.len());
        rest = &after[skip..];
    }
    out.push_str(rest);
    out
}

fn redact_emails(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' && i > 0 {
            let mut start = i;
            while start > 0 && is_email_byte(bytes[start - 1]) {
                start -= 1;
            }
            let mut end = i + 1;
            while end < bytes.len() && is_email_byte(bytes[end]) {
                end += 1;
            }
            if start < i && end > i + 1 && bytes[i + 1] != b'.' {
                out.truncate(out.len().saturating_sub(i - start));
                out.push_str("[REDACTED:email]");
                i = end;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn is_email_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-' || b == b'+'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canary_and_secrets_are_stripped() {
        let raw = format!(
            "token Bearer abc.def sk-live-123 user@example.com canary={DEFAULT_SECRET_CANARY}"
        );
        let redacted = redact_text(&raw);
        assert!(!redacted.contains(DEFAULT_SECRET_CANARY));
        assert!(!redacted.contains("Bearer abc"));
        assert!(!redacted.contains("sk-live"));
        assert!(!redacted.contains("user@example.com"));
        assert!(redacted.contains(CANARY_PLACEHOLDER));
        assert!(redacted.contains("[REDACTED:bearer]"));
        assert!(redacted.contains("[REDACTED:secret]"));
        assert!(redacted.contains("[REDACTED:email]"));
    }
}
