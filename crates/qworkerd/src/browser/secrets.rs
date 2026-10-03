//! Secret placeholders and sensitive fields (ADP-001, DOMAIN.md §16).
//!
//! The model never holds a credential. To fill a login it writes `{{secret:<name>.<field>}}` where the
//! value would go, and the trusted runtime substitutes the real value at the moment of insertion, from
//! the secret broker's handles — this module does not invent a store, it only names the seam.
//!
//! Four properties are the point, and each is enforced here or in the driver that calls it:
//!
//! * **the value has no rendering.** [`SecretText`] prints as a redacted marker and has no `Display`,
//!   `Clone` or `Serialize`; the only way to get the characters is [`SecretText::expose`], which the
//!   insertion code calls once. A secret therefore cannot reach a log, an event, a ledger digest or a
//!   model-facing result by being formatted into one.
//! * **the digest uses the placeholder form.** The canonical-argument digest of an action is computed
//!   from the arguments the model wrote, never from the substituted text.
//! * **the origin is checked live.** Resolving a placeholder yields the origins the secret was issued
//!   for; the driver compares them with the top frame's and the target frame's origin immediately
//!   before insertion.
//! * **plain text does not go into a secret field.** A password, one-time-code or card field accepts
//!   only placeholders from the model; a literal value there is refused.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;

use super::fault::{BrowserFault, FaultCode};

/// A resolved secret value: no `Display`, no `Clone`, no `Serialize`, and a redacted `Debug`.
pub struct SecretText(String);

impl SecretText {
    /// Wrap a value the runtime resolved.
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// The characters. The caller is at the insertion boundary; nothing else should be.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretText(<{} bytes redacted>)", self.0.len())
    }
}

/// What the runtime returns for one placeholder: the value, and where it may be used.
#[derive(Debug)]
pub struct ResolvedSecret {
    /// The value. Redacted in every rendering.
    pub value: SecretText,
    /// The origins the secret was issued for (`https://example.com`). Empty means *nowhere*.
    pub allowed_origins: Vec<String>,
}

/// One placeholder to resolve, with the call it is resolved for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRequest {
    /// The secret's name (the handle label).
    pub name: String,
    /// The field within it (`username`, `password`, …).
    pub field: String,
    /// The ToolCall the insertion belongs to.
    pub call_id: String,
    /// The action id the insertion belongs to.
    pub action_id: String,
}

/// The future a resolution returns.
pub type ResolveFuture =
    Pin<Box<dyn Future<Output = Result<ResolvedSecret, BrowserFault>> + Send + 'static>>;

/// The seam to the runtime's secret broker.
///
/// It is a function value rather than a trait so the worker holds no second abstraction of a secret
/// store: whatever installs it (the control channel in production, a broker-backed closure in a test)
/// is the runtime half of the boundary, and this crate sees only a request and a bounded answer.
#[derive(Clone)]
pub struct SecretPort {
    resolve: Arc<dyn Fn(SecretRequest) -> ResolveFuture + Send + Sync>,
}

impl SecretPort {
    /// Install a resolver.
    pub fn new<F>(resolve: F) -> Self
    where
        F: Fn(SecretRequest) -> ResolveFuture + Send + Sync + 'static,
    {
        Self {
            resolve: Arc::new(resolve),
        }
    }

    /// Resolve one placeholder.
    ///
    /// # Errors
    /// Returns whatever the runtime refused with, normally [`FaultCode::SecretUnavailable`].
    pub async fn resolve(&self, request: SecretRequest) -> Result<ResolvedSecret, BrowserFault> {
        (self.resolve)(request).await
    }
}

impl std::fmt::Debug for SecretPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretPort(..)")
    }
}

/// One piece of a text argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// Characters the model wrote.
    Literal(String),
    /// A `{{secret:<name>.<field>}}` placeholder.
    Secret {
        /// The secret's name.
        name: String,
        /// The field.
        field: String,
    },
}

const OPEN: &str = "{{secret:";

/// Split a text argument into literals and placeholders.
///
/// # Errors
/// Returns [`FaultCode::InvalidRequest`] for a placeholder that is opened and not closed, or whose name
/// or field is empty or uses characters outside `[A-Za-z0-9_-]`. A half-written placeholder is refused
/// rather than typed, because typing it would put a secret's name into a page.
pub fn parse_placeholders(text: &str) -> Result<Vec<Segment>, BrowserFault> {
    let invalid = |why: &str| {
        BrowserFault::new(
            FaultCode::InvalidRequest,
            format!("malformed secret placeholder: {why}"),
        )
    };
    let mut segments = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        if start > 0 {
            segments.push(Segment::Literal(rest[..start].to_string()));
        }
        let after = &rest[start + OPEN.len()..];
        let Some(end) = after.find("}}") else {
            return Err(invalid("it is never closed"));
        };
        let body = &after[..end];
        let Some((name, field)) = body.split_once('.') else {
            return Err(invalid("it must be {{secret:<name>.<field>}}"));
        };
        let valid = |part: &str| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        };
        if !valid(name) || !valid(field) {
            return Err(invalid(
                "the name and field are letters, digits, '_' and '-'",
            ));
        }
        segments.push(Segment::Secret {
            name: name.to_string(),
            field: field.to_string(),
        });
        rest = &after[end + 2..];
    }
    if !rest.is_empty() {
        segments.push(Segment::Literal(rest.to_string()));
    }
    Ok(segments)
}

/// Whether any segment is a placeholder.
#[must_use]
pub fn has_placeholders(segments: &[Segment]) -> bool {
    segments
        .iter()
        .any(|segment| matches!(segment, Segment::Secret { .. }))
}

/// Whether the text is nothing but placeholders (no literal characters at all).
#[must_use]
pub fn only_placeholders(segments: &[Segment]) -> bool {
    !segments.is_empty()
        && segments
            .iter()
            .all(|segment| matches!(segment, Segment::Secret { .. }))
}

/// Why a field is sensitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensitiveKind {
    /// A password.
    Password,
    /// A one-time code.
    OneTimeCode,
    /// A card number.
    CardNumber,
    /// A card security code.
    CardSecurityCode,
    /// A card expiry.
    CardExpiry,
}

impl SensitiveKind {
    /// The canonical spelling the snapshot and the denial both use.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::OneTimeCode => "one_time_code",
            Self::CardNumber => "card_number",
            Self::CardSecurityCode => "card_security_code",
            Self::CardExpiry => "card_expiry",
        }
    }
}

/// Decide whether an element is a sensitive input from what the page says about it.
///
/// The decisive signals are the ones the browser itself acts on: `type=password` and the standard
/// `autocomplete` tokens. A few name-based hints are added for fields that carry neither (a card number
/// in a plain text input called `cardnumber`) because those are exactly the fields a page leaves
/// unlabelled. A field that merely *might* be sensitive is treated as sensitive: the cost of a false
/// positive is that the model uses a placeholder, and the cost of a false negative is a card number in
/// a transcript.
#[must_use]
pub fn sensitivity_of(descriptor: &Value) -> Option<SensitiveKind> {
    let text = |key: &str| {
        descriptor
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase()
    };
    let input_type = text("type");
    let autocomplete = text("autocomplete");
    if input_type == "password" {
        return Some(SensitiveKind::Password);
    }
    for token in autocomplete.split_whitespace() {
        match token {
            "current-password" | "new-password" => return Some(SensitiveKind::Password),
            "one-time-code" => return Some(SensitiveKind::OneTimeCode),
            "cc-number" => return Some(SensitiveKind::CardNumber),
            "cc-csc" => return Some(SensitiveKind::CardSecurityCode),
            "cc-exp" | "cc-exp-month" | "cc-exp-year" => return Some(SensitiveKind::CardExpiry),
            _ => {}
        }
    }
    let hint = format!(
        "{} {} {} {}",
        text("name"),
        text("id"),
        text("placeholder"),
        text("aria_label")
    );
    // Short abbreviations are matched as whole words (`otp`, `cvv`), so `footprint` is not a code field;
    // longer phrases are matched as substrings once separators are removed.
    let words: Vec<&str> = hint
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let word = |needles: &[&str]| words.iter().any(|candidate| needles.contains(candidate));
    let squashed = hint.replace(['_', '-', ' '], "");
    let contains = |needles: &[&str]| needles.iter().any(|needle| squashed.contains(needle));
    if word(&["otp", "totp", "hotp", "2fa", "mfa"])
        || contains(&[
            "onetimecode",
            "verificationcode",
            "authcode",
            "2facode",
            "mfacode",
        ])
    {
        return Some(SensitiveKind::OneTimeCode);
    }
    if contains(&["cardnumber", "ccnumber", "ccnum", "creditcard", "cardnum"]) {
        return Some(SensitiveKind::CardNumber);
    }
    if word(&["cvc", "cvv", "csc"]) || contains(&["securitycode", "cardverification"]) {
        return Some(SensitiveKind::CardSecurityCode);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn placeholders_split_around_literals() {
        let segments = parse_placeholders("user={{secret:acme.username}}!").expect("parse");
        assert_eq!(
            segments,
            vec![
                Segment::Literal("user=".to_string()),
                Segment::Secret {
                    name: "acme".to_string(),
                    field: "username".to_string()
                },
                Segment::Literal("!".to_string()),
            ]
        );
        assert!(has_placeholders(&segments));
        assert!(!only_placeholders(&segments));
        assert!(only_placeholders(
            &parse_placeholders("{{secret:a.b}}{{secret:c.d}}").expect("parse")
        ));
        assert!(!has_placeholders(
            &parse_placeholders("plain").expect("parse")
        ));
    }

    #[test]
    fn a_half_written_placeholder_is_refused_not_typed() {
        for bad in [
            "{{secret:acme.password",
            "{{secret:acme}}",
            "{{secret:.password}}",
            "{{secret:ac me.pw}}",
        ] {
            assert!(parse_placeholders(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_secret_has_no_rendering() {
        let secret = SecretText::new("hunter2-canary".to_string());
        assert!(!format!("{secret:?}").contains("hunter2"));
    }

    #[test]
    fn sensitivity_follows_the_signals_the_browser_acts_on() {
        let kind = |value: Value| sensitivity_of(&value);
        assert_eq!(
            kind(json!({"type": "password"})),
            Some(SensitiveKind::Password)
        );
        assert_eq!(
            kind(json!({"type": "text", "autocomplete": "one-time-code"})),
            Some(SensitiveKind::OneTimeCode)
        );
        assert_eq!(
            kind(json!({"type": "text", "autocomplete": "section-x cc-number"})),
            Some(SensitiveKind::CardNumber)
        );
        assert_eq!(
            kind(json!({"type": "text", "autocomplete": "cc-csc"})),
            Some(SensitiveKind::CardSecurityCode)
        );
        assert_eq!(
            kind(json!({"type": "tel", "name": "card_number"})),
            Some(SensitiveKind::CardNumber)
        );
        assert_eq!(kind(json!({"type": "text", "name": "username"})), None);
        assert_eq!(
            kind(json!({"type": "email", "autocomplete": "email"})),
            None
        );
    }
}
