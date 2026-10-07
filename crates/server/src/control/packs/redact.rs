//! Redaction of an install diagnostic before it is stored or returned (ADP-008, DOMAIN.md 11.6 C).
//!
//! A diagnostic is built from many things: a fetch error that names a path, a plane finding that names a tool
//! the pack asked for, a database error. Any of them may carry what must never be stored: a secret, a token, a URL
//! with credentials in it, a home directory (which names a person). Redaction runs here, at the one place a
//! diagnostic enters a report, whoever wrote it, and is idempotent. Order matters: URLs and paths first (their
//! credentials and user names are structure the token rules cannot see), then the recording rule set `v1` (provider
//! keys, bearer values, card numbers, verification codes), then configured canaries.

use std::sync::LazyLock;

use regex::Regex;

use crate::control::skills::recording::redact::redact_text as redact_secrets;
use crate::observability::{secret_canaries, CANARY_PLACEHOLDER};

static URL_USERINFO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b([a-z][a-z0-9+.-]*://)[^/\s@?#]+@").expect("url userinfo"));
static QUERY_SECRET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)([?&;](?:access[_-]?token|api[_-]?key|auth|key|password|passwd|pwd|secret|sig|signature|token)=)[^&\s#;]+",
    )
    .expect("query secret")
});
static HOME_UNIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:/Users|/home)/[^/\s:;,'\x22]+").expect("home"));
static HOME_ROOT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/root\b").expect("root home"));
static HOME_WINDOWS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)[a-z]:\\Users\\[^\\\s:;,'\x22]+").expect("windows home"));
static KEY_VALUE_SECRET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(password|passwd|secret|api[_-]?key|token)\s*[:=]\s*[^\s,;&\x22']+")
        .expect("key value secret")
});

/// What replaces the user info of a URL.
pub const URL_MARKER: &str = "[REDACTED:url_credentials]";
/// What replaces a secret-named query value or `key=value` pair's value.
pub const VALUE_MARKER: &str = "[REDACTED:secret_value]";
/// What replaces a home directory.
pub const HOME_MARKER: &str = "~";

/// Redact a diagnostic's text.
#[must_use]
pub fn redact_diagnostic(text: &str) -> String {
    let mut out = URL_USERINFO
        .replace_all(text, |captures: &regex::Captures<'_>| {
            format!("{}{URL_MARKER}@", &captures[1])
        })
        .into_owned();
    out = QUERY_SECRET
        .replace_all(&out, |captures: &regex::Captures<'_>| {
            format!("{}{VALUE_MARKER}", &captures[1])
        })
        .into_owned();
    out = HOME_UNIX.replace_all(&out, HOME_MARKER).into_owned();
    out = HOME_ROOT.replace_all(&out, HOME_MARKER).into_owned();
    out = HOME_WINDOWS.replace_all(&out, HOME_MARKER).into_owned();
    out = KEY_VALUE_SECRET
        .replace_all(&out, |captures: &regex::Captures<'_>| {
            format!("{}={VALUE_MARKER}", &captures[1])
        })
        .into_owned();
    let (mut out, _) = redact_secrets(&out);
    for canary in secret_canaries() {
        if !canary.is_empty() {
            out = out.replace(&canary, CANARY_PLACEHOLDER);
        }
    }
    out
}

/// Cut `text` to at most `max` bytes on a character boundary, marking the cut.
#[must_use]
pub fn bound(text: &str, max: usize) -> String {
    const CUT: &str = "...[truncated]";
    if text.len() <= max {
        return text.to_string();
    }
    // A bound too small for the marker is a plain cut.
    let keep = if max > CUT.len() {
        max - CUT.len()
    } else {
        max
    };
    let marker = if max > CUT.len() { CUT } else { "" };
    let mut end = keep.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{marker}", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_homes_secrets_and_canaries_are_removed_and_the_rest_is_kept() {
        let canary = secret_canaries().remove(0);
        let text = format!(
            "fetch https://deploy:{canary}@packs.example.test/p.tgz?token=abc123&x=1 failed at \
             /Users/jane.doe/work/packs/coding/pack.yaml and C:\\Users\\Jane\\p.yaml; password: hunter2; \
             Bearer abcdefghijklmnop; AKIAIOSFODNN7EXAMPLE; keep this tool name fs.read"
        );
        let out = redact_diagnostic(&text);
        for gone in [
            canary.as_str(),
            "deploy",
            "abc123",
            "jane.doe",
            "Jane",
            "hunter2",
            "abcdefghijklmnop",
            "AKIAIOSFODNN7EXAMPLE",
        ] {
            assert!(!out.contains(gone), "{gone} survived: {out}");
        }
        assert!(out.contains("packs.example.test") && out.contains("x=1"));
        assert!(out.contains("fs.read") && out.contains("~/work/packs/coding/pack.yaml"));
        // Idempotent: a redacted text redacts to itself.
        assert_eq!(redact_diagnostic(&out), out);
    }

    #[test]
    fn a_bound_never_splits_a_character() {
        let text = "é".repeat(100);
        for max in [10, 11, 25, 64] {
            let cut = bound(&text, max);
            assert!(cut.len() <= max, "{} > {max}", cut.len());
            assert!(cut.ends_with("...[truncated]") || max <= 14);
        }
        assert_eq!(bound("short", 64), "short");
    }
}
