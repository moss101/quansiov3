//! The recording redaction rule set `v1` (DOMAIN.md §11.7 C).
//!
//! Redaction removes what must never be stored or sent: provider-shaped keys and tokens, bearer
//! credentials, card numbers, verification codes and anything typed into a password field. Each match
//! is replaced by `[REDACTED:<category>]` and counted; the counts are the only thing that survives.
//!
//! The same rule set runs in three places that must agree: the person's recorder (`native/macos`), this
//! server before an archive is stored, and the intelligence plane before a compiler reads one. They are
//! pinned to one hand-written corpus (`tests/contract/fixtures/recording/redaction_corpus.json`), and
//! the vocabulary below is the subset of regular expressions all three engines read the same way: no
//! lookaround, no flags beyond case folding.
//!
//! Redaction is idempotent (a marker matches no rule), so the server runs the same function as a scan
//! and refuses an archive for which it still finds something.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{json, Value};

/// The rule-set version an archive declares and `config/skill-recording.yaml` pins.
pub const RULE_SET: &str = "v1";

/// What redaction removes, in the order the rules run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    /// Provider-shaped keys, tokens, JSON web tokens and private-key blocks.
    ApiKey,
    /// `Bearer …` and `Authorization:` values.
    BearerToken,
    /// Thirteen or more digits, with single spaces or dashes between them.
    CardNumber,
    /// A short digit run next to a word such as code, otp or 2fa, and text typed into a control so named.
    VerificationCode,
    /// Text typed into a secure or password control.
    PasswordField,
}

impl Category {
    /// Every category, in rule order.
    pub const ALL: [Self; 5] = [
        Self::ApiKey,
        Self::BearerToken,
        Self::CardNumber,
        Self::VerificationCode,
        Self::PasswordField,
    ];

    /// The durable spelling (the key in a redaction summary).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::BearerToken => "bearer_token",
            Self::CardNumber => "card_number",
            Self::VerificationCode => "verification_code",
            Self::PasswordField => "password_field",
        }
    }

    /// What replaces a removed value.
    #[must_use]
    pub fn marker(self) -> String {
        format!("[REDACTED:{}]", self.as_str())
    }

    /// Parse the durable spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|category| category.as_str() == value)
    }
}

/// How many values of each category a text held.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts(BTreeMap<Category, u64>);

impl Counts {
    /// Count one more value of `category`.
    pub fn add(&mut self, category: Category, n: u64) {
        if n > 0 {
            *self.0.entry(category).or_insert(0) += n;
        }
    }

    /// How many values of `category` were found.
    #[must_use]
    pub fn get(&self, category: Category) -> u64 {
        self.0.get(&category).copied().unwrap_or(0)
    }

    /// Whether nothing was found.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.values().all(|n| *n == 0)
    }

    /// The categories that have a non-zero count, in rule order.
    #[must_use]
    pub fn categories(&self) -> Vec<Category> {
        Category::ALL
            .into_iter()
            .filter(|category| self.get(*category) > 0)
            .collect()
    }

    /// Fold another count into this one.
    pub fn merge(&mut self, other: &Counts) {
        for (category, n) in &other.0 {
            self.add(*category, *n);
        }
    }

    /// The summary object: every category present, zero included.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut map = serde_json::Map::new();
        for category in Category::ALL {
            map.insert(category.as_str().to_string(), json!(self.get(category)));
        }
        Value::Object(map)
    }
}

fn rx(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|error| panic!("redaction pattern {pattern:?}: {error}"))
}

const TRIGGERS: &str = r"(?:otp|passcode|2fa|mfa|pin|code|verification|one[- ]?time|confirmation)";

static PEM_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    rx(r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----")
});
static PEM_HEADER: LazyLock<Regex> = LazyLock::new(|| rx(r"-----BEGIN [A-Z ]*PRIVATE KEY-----"));
static API_KEY_SHAPES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"\b(?:sk|pk|rk|whsec)[_-][A-Za-z0-9_-]{16,}",
        r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
        r"\bgh[pousr]_[A-Za-z0-9]{30,}",
        r"\bgithub_pat_[A-Za-z0-9_]{40,}",
        r"\bxox[abprs]-[A-Za-z0-9-]{10,}",
        r"\bAIza[0-9A-Za-z_-]{35,}",
        r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
    ]
    .iter()
    .map(|pattern| rx(pattern))
    .collect()
});
static LONG_TOKEN: LazyLock<Regex> = LazyLock::new(|| rx(r"\b[A-Za-z0-9_-]{40,}\b"));
static AUTHORIZATION: LazyLock<Regex> = LazyLock::new(|| {
    rx(r"(?i)\bauthorization\s*[:=]\s*(?:(?:bearer|basic|token)\s+)?[A-Za-z0-9._~+/=-]{8,}")
});
static BEARER: LazyLock<Regex> = LazyLock::new(|| rx(r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{8,}"));
static CARD: LazyLock<Regex> = LazyLock::new(|| rx(r"\d(?:[ -]?\d){12,}"));
static CODE_AFTER_WORD: LazyLock<Regex> =
    LazyLock::new(|| rx(&format!(r"(?i)\b{TRIGGERS}\b[^0-9\n]{{0,40}}\b\d{{4,8}}\b")));
static CODE_BEFORE_WORD: LazyLock<Regex> =
    LazyLock::new(|| rx(&format!(r"(?i)\b\d{{4,8}}\b[^0-9\n]{{0,40}}\b{TRIGGERS}\b")));

fn replace_all(text: &str, pattern: &Regex, category: Category, counts: &mut Counts) -> String {
    let marker = category.marker();
    let mut found = 0u64;
    let out = pattern
        .replace_all(text, |_: &regex::Captures<'_>| {
            found += 1;
            marker.clone()
        })
        .into_owned();
    counts.add(category, found);
    out
}

/// A long run that is a token and not a word: it holds both a letter and a digit.
fn replace_long_tokens(text: &str, counts: &mut Counts) -> String {
    let marker = Category::ApiKey.marker();
    let mut found = 0u64;
    let out = LONG_TOKEN
        .replace_all(text, |captures: &regex::Captures<'_>| {
            let token = &captures[0];
            let letter = token.chars().any(|c| c.is_ascii_alphabetic());
            let digit = token.chars().any(|c| c.is_ascii_digit());
            if letter && digit {
                found += 1;
                marker.clone()
            } else {
                token.to_string()
            }
        })
        .into_owned();
    counts.add(Category::ApiKey, found);
    out
}

/// Redact free text: the text with every match replaced, and what was found.
#[must_use]
pub fn redact_text(text: &str) -> (String, Counts) {
    let mut counts = Counts::default();
    let mut out = replace_all(text, &PEM_BLOCK, Category::ApiKey, &mut counts);
    out = replace_all(&out, &PEM_HEADER, Category::ApiKey, &mut counts);
    for shape in API_KEY_SHAPES.iter() {
        out = replace_all(&out, shape, Category::ApiKey, &mut counts);
    }
    out = replace_long_tokens(&out, &mut counts);
    out = replace_all(&out, &AUTHORIZATION, Category::BearerToken, &mut counts);
    out = replace_all(&out, &BEARER, Category::BearerToken, &mut counts);
    out = replace_all(&out, &CARD, Category::CardNumber, &mut counts);
    out = replace_all(
        &out,
        &CODE_AFTER_WORD,
        Category::VerificationCode,
        &mut counts,
    );
    out = replace_all(
        &out,
        &CODE_BEFORE_WORD,
        Category::VerificationCode,
        &mut counts,
    );
    (out, counts)
}

/// What a text still holds that redaction would remove. Empty for any redacted text.
#[must_use]
pub fn scan_text(text: &str) -> Counts {
    redact_text(text).1
}

/// The accessibility attributes of the control text is entered into.
#[derive(Debug, Clone, Copy, Default)]
pub struct FieldAttrs<'a> {
    /// `AXRole`.
    pub role: Option<&'a str>,
    /// `AXSubrole`.
    pub subrole: Option<&'a str>,
    /// `AXTitle`.
    pub title: Option<&'a str>,
    /// `AXIdentifier`.
    pub identifier: Option<&'a str>,
    /// `AXDescription`.
    pub description: Option<&'a str>,
}

/// What entering text into a control means for that text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldRule {
    /// A secure text field: whatever was typed is dropped whole.
    Secure,
    /// A control whose label says what it holds: the text is replaced by that category's marker.
    Labelled(Category),
}

const SECURE_ROLE: &str = "AXSecureTextField";

fn label_tokens(label: &str) -> Vec<String> {
    let mut spaced = String::with_capacity(label.len() + 4);
    let mut previous: Option<char> = None;
    for c in label.chars() {
        if let Some(p) = previous {
            if (p.is_ascii_lowercase() || p.is_ascii_digit()) && c.is_ascii_uppercase() {
                spaced.push(' ');
            }
        }
        spaced.push(c);
        previous = Some(c);
    }
    spaced
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn has_any(tokens: &[String], words: &[&str]) -> bool {
    tokens.iter().any(|token| words.contains(&token.as_str()))
}

fn has_pair(tokens: &[String], pairs: &[(&str, &str)]) -> bool {
    tokens
        .windows(2)
        .any(|w| pairs.iter().any(|(a, b)| w[0] == *a && w[1] == *b))
}

fn label_category(tokens: &[String]) -> Option<Category> {
    if has_any(
        tokens,
        &[
            "otp",
            "2fa",
            "mfa",
            "verification",
            "authenticator",
            "onetime",
        ],
    ) || has_pair(
        tokens,
        &[
            ("one", "time"),
            ("security", "code"),
            ("auth", "code"),
            ("authentication", "code"),
            ("confirmation", "code"),
            ("sms", "code"),
            ("login", "code"),
        ],
    ) {
        return Some(Category::VerificationCode);
    }
    if has_any(tokens, &["cvv", "cvc", "ccnum", "cardnumber"])
        || has_pair(
            tokens,
            &[
                ("card", "number"),
                ("card", "num"),
                ("credit", "card"),
                ("debit", "card"),
                ("cc", "number"),
            ],
        )
    {
        return Some(Category::CardNumber);
    }
    if has_any(tokens, &["apikey"])
        || has_pair(
            tokens,
            &[
                ("api", "key"),
                ("api", "token"),
                ("access", "token"),
                ("auth", "token"),
                ("private", "key"),
            ],
        )
    {
        return Some(Category::ApiKey);
    }
    if has_any(
        tokens,
        &[
            "password",
            "passwd",
            "pwd",
            "passphrase",
            "passcode",
            "secret",
        ],
    ) {
        return Some(Category::PasswordField);
    }
    None
}

/// What a control's own attributes say about text entered into it, if anything.
#[must_use]
pub fn field_rule(attrs: &FieldAttrs<'_>) -> Option<FieldRule> {
    if attrs.role == Some(SECURE_ROLE) || attrs.subrole == Some(SECURE_ROLE) {
        return Some(FieldRule::Secure);
    }
    let labels: Vec<Vec<String>> = [attrs.title, attrs.identifier, attrs.description]
        .into_iter()
        .flatten()
        .map(label_tokens)
        .collect();
    Category::ALL
        .into_iter()
        .filter(|category| *category != Category::BearerToken)
        .find(|category| {
            labels
                .iter()
                .any(|tokens| label_category(tokens) == Some(*category))
        })
        .map(FieldRule::Labelled)
}

/// Redact text entered into a control: dropped (`None`) in a secure field, the category marker in a
/// labelled one, otherwise the ordinary text rules. Empty text is nothing and counts nothing.
#[must_use]
pub fn redact_field_text(attrs: &FieldAttrs<'_>, text: &str) -> (Option<String>, Counts) {
    if text.is_empty() {
        return (Some(String::new()), Counts::default());
    }
    let mut counts = Counts::default();
    match field_rule(attrs) {
        Some(FieldRule::Secure) => {
            counts.add(Category::PasswordField, 1);
            (None, counts)
        }
        Some(FieldRule::Labelled(category)) => {
            counts.add(category, 1);
            (Some(category.marker()), counts)
        }
        None => {
            let (redacted, counts) = redact_text(text);
            (Some(redacted), counts)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus() -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/contract/fixtures/recording/redaction_corpus.json");
        serde_json::from_str(&std::fs::read_to_string(&path).expect("corpus")).expect("json")
    }

    fn expected_counts(case: &Value) -> Counts {
        let mut counts = Counts::default();
        for (key, n) in case["counts"].as_object().expect("counts") {
            counts.add(
                Category::parse(key).expect("category"),
                n.as_u64().expect("n"),
            );
        }
        counts
    }

    fn attrs(field: &Value) -> FieldAttrs<'_> {
        FieldAttrs {
            role: field["ax_role"].as_str(),
            subrole: field["ax_subrole"].as_str(),
            title: field["ax_title"].as_str(),
            identifier: field["ax_identifier"].as_str(),
            description: field["ax_description"].as_str(),
        }
    }

    #[test]
    fn the_shared_corpus_agrees() {
        let corpus = corpus();
        assert_eq!(corpus["rule_set"], RULE_SET);
        let cases = corpus["cases"].as_array().expect("cases");
        assert!(
            cases.len() >= 40,
            "the corpus is the contract: {}",
            cases.len()
        );
        for case in cases {
            let id = case["id"].as_str().expect("id");
            let input = case["input"].as_str().expect("input");
            let (redacted, counts) = if case.get("field").is_some() {
                let (text, counts) = redact_field_text(&attrs(&case["field"]), input);
                (text, counts)
            } else {
                let (text, counts) = redact_text(input);
                (Some(text), counts)
            };
            assert_eq!(
                redacted.as_deref(),
                case["redacted"].as_str(),
                "{id}: redacted text"
            );
            assert_eq!(counts, expected_counts(case), "{id}: counts");
            // Idempotent: what redaction produced redacts to itself, and a scan finds nothing.
            if let Some(text) = redacted {
                assert_eq!(redact_text(&text).0, text, "{id}: not idempotent");
                assert!(
                    scan_text(&text).is_empty(),
                    "{id}: a scan of the output finds more"
                );
            }
        }
    }

    #[test]
    fn counts_carry_every_category_and_never_a_value() {
        let (text, counts) = redact_text("AKIAIOSFODNN7EXAMPLE and 4111111111111111");
        assert_eq!(text, "[REDACTED:api_key] and [REDACTED:card_number]");
        let summary = counts.to_json();
        for category in Category::ALL {
            assert!(summary.get(category.as_str()).is_some(), "{category:?}");
        }
        let rendered = summary.to_string();
        assert!(!rendered.contains("AKIA") && !rendered.contains("4111"));
    }

    #[test]
    fn camel_case_labels_are_split_before_they_are_read() {
        let attrs = FieldAttrs {
            identifier: Some("passwordField"),
            ..FieldAttrs::default()
        };
        assert_eq!(
            field_rule(&attrs),
            Some(FieldRule::Labelled(Category::PasswordField))
        );
        let attrs = FieldAttrs {
            title: Some("Footprint"),
            ..FieldAttrs::default()
        };
        assert_eq!(
            field_rule(&attrs),
            None,
            "otp inside a word is not a code field"
        );
    }
}
