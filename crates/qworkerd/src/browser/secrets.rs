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

impl Drop for SecretText {
    /// Overwrite the characters before the allocation is released, so a freed secret is not left in memory that is reused as
    /// it was (best effort: the optimizer is told the zeroed bytes are observed).
    fn drop(&mut self) {
        let mut bytes = std::mem::take(&mut self.0).into_bytes();
        bytes.fill(0);
        std::hint::black_box(&bytes);
    }
}

/// A payment field the agent may name (DOMAIN.md §13.9 C). The agent never writes a value: it names one of these, and the
/// runtime inserts what the person supplied for it, into a field whose live `autocomplete` token says it is that field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentField {
    /// `cc-number`.
    CardNumber,
    /// `cc-csc`.
    Cvc,
    /// `cc-exp`.
    Expiry,
    /// `cc-exp-month`.
    ExpiryMonth,
    /// `cc-exp-year`.
    ExpiryYear,
    /// `cc-name`.
    CardHolderName,
    /// `address-line1` or `street-address`.
    AddressLine,
    /// `address-level2`.
    City,
    /// `address-level1`.
    Region,
    /// `postal-code`.
    PostalCode,
    /// `country` or `country-name`.
    Country,
}

impl PaymentField {
    /// Every field, in the order the tool's schema lists them.
    pub const ALL: [Self; 11] = [
        Self::CardNumber,
        Self::Cvc,
        Self::Expiry,
        Self::ExpiryMonth,
        Self::ExpiryYear,
        Self::CardHolderName,
        Self::AddressLine,
        Self::City,
        Self::Region,
        Self::PostalCode,
        Self::Country,
    ];

    /// The canonical spelling the tool takes and the person's supply names.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CardNumber => "card_number",
            Self::Cvc => "cvc",
            Self::Expiry => "expiry",
            Self::ExpiryMonth => "expiry_month",
            Self::ExpiryYear => "expiry_year",
            Self::CardHolderName => "card_holder_name",
            Self::AddressLine => "address_line",
            Self::City => "city",
            Self::Region => "region",
            Self::PostalCode => "postal_code",
            Self::Country => "country",
        }
    }

    /// Parse the canonical spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|field| field.as_str() == value)
    }

    /// The `autocomplete` tokens that identify the field. A field the page does not label with one of them is not accepted: the
    /// runtime does not guess from a name or a placeholder where a card is concerned.
    #[must_use]
    pub const fn tokens(self) -> &'static [&'static str] {
        match self {
            Self::CardNumber => &["cc-number"],
            Self::Cvc => &["cc-csc"],
            Self::Expiry => &["cc-exp"],
            Self::ExpiryMonth => &["cc-exp-month"],
            Self::ExpiryYear => &["cc-exp-year"],
            Self::CardHolderName => &["cc-name"],
            Self::AddressLine => &["address-line1", "street-address"],
            Self::City => &["address-level2"],
            Self::Region => &["address-level1"],
            Self::PostalCode => &["postal-code"],
            Self::Country => &["country", "country-name"],
        }
    }

    /// Whether the field is masked on the page before the value goes in: the card number, its security code and its expiry.
    #[must_use]
    pub const fn masked(self) -> bool {
        matches!(
            self,
            Self::CardNumber | Self::Cvc | Self::Expiry | Self::ExpiryMonth | Self::ExpiryYear
        )
    }
}

/// One payment value to take, with the call it is taken for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentRequest {
    /// The field the agent named.
    pub field: PaymentField,
    /// The ToolCall the insertion belongs to.
    pub call_id: String,
    /// The action id the insertion belongs to.
    pub action_id: String,
}

/// A payment value the person supplied for one use: the value, and the one origin it may be inserted on.
#[derive(Debug)]
pub struct PaymentValue {
    /// The value. Redacted in every rendering, zeroed when dropped.
    pub value: SecretText,
    /// The origin the person supplied it for (`https://shop.example`).
    pub allowed_origin: String,
}

/// The future a take returns.
pub type TakeFuture =
    Pin<Box<dyn Future<Output = Result<PaymentValue, BrowserFault>> + Send + 'static>>;

type TakeFn = Arc<dyn Fn(PaymentRequest) -> TakeFuture + Send + Sync>;

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
    /// Where the person's one-time payment values are taken from (ADP-009), when the runtime has one.
    payments: Option<TakeFn>,
}

impl SecretPort {
    /// Install a resolver.
    pub fn new<F>(resolve: F) -> Self
    where
        F: Fn(SecretRequest) -> ResolveFuture + Send + Sync + 'static,
    {
        Self {
            resolve: Arc::new(resolve),
            payments: None,
        }
    }

    /// Install the source of one-time payment values. A take **consumes**: the runtime deletes the value as it hands it over,
    /// so a second take of the same field is `SecretUnavailable` until the person supplies it again.
    #[must_use]
    pub fn with_payments<F>(mut self, take: F) -> Self
    where
        F: Fn(PaymentRequest) -> TakeFuture + Send + Sync + 'static,
    {
        self.payments = Some(Arc::new(take));
        self
    }

    /// Take one payment value, if the runtime has any to give.
    ///
    /// # Errors
    /// Returns [`FaultCode::SecretUnavailable`] when no source is connected, and whatever the runtime refused with otherwise
    /// (nothing supplied, expired, already used).
    pub async fn take_payment(
        &self,
        request: PaymentRequest,
    ) -> Result<PaymentValue, BrowserFault> {
        match &self.payments {
            Some(take) => take(request).await,
            None => Err(BrowserFault::new(
                FaultCode::SecretUnavailable,
                "no payment input is connected to this session",
            )),
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
