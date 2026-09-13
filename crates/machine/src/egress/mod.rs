//! The egress broker: which destinations an execution target may reach (EXEC-008, DOSSIER.md §5
//! "Egress", DOMAIN.md §7.1 `network.egress.new_destination`, §8.2 `network_policy_id`).
//!
//! The broker answers one question — *may this target reach this destination?* — and its answer is
//! **deny**. Nothing is reachable because it was not forbidden: a destination is reachable only when a
//! live, scoped, unexpired grant covers it, and every address the name resolves to is one the target's
//! network policy allows. `EgressBroker::decide` is the single entry point, and the browser, the
//! connector broker and webhook delivery are all callers of it rather than owners of their own rules.
//!
//! Three properties are the point:
//!
//! * **the name is not the destination.** A grant is issued for a host, but the decision is taken
//!   against the addresses the host resolves to *now*, so a granted name that starts resolving to a
//!   loopback or private address is refused (DNS rebinding). An empty resolution is refused too: not
//!   knowing the addresses is not the same as knowing they are fine.
//! * **a policy change fences grants rather than racing them.** A grant records the
//!   `policies.version` it was issued under, so incrementing the version fences every grant at the old
//!   revision in one comparison — no scan, no window in which a stale grant is still live.
//! * **the log holds no secrets.** A decision record is built from the destination, the decision and
//!   identifiers only. The request carries headers (which is where a credential would be), and no
//!   header value reaches the record.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

mod store;

pub use store::{EgressError, EgressGrant, EgressStore, InstallOutcome, NewGrant};

/// What kind of host a destination names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKind {
    /// A DNS name, which must be resolved before it is reached.
    Name,
    /// An IP literal, which needs no resolution.
    Literal,
}

/// Where an egress request came from. Recorded so the log says who asked, never used to bypass a
/// rule: the same destination reaches the same decision whichever caller asked (EXEC-008's "webhook
/// and connector egress use the same broker").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressOrigin {
    /// A tool call running in the target.
    Tool,
    /// A managed browser session navigating.
    Browser,
    /// A connector or integration call.
    Connector,
    /// Outbound webhook delivery.
    Webhook,
}

impl EgressOrigin {
    /// Canonical wire and log value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Browser => "browser",
            Self::Connector => "connector",
            Self::Webhook => "webhook",
        }
    }
}

/// How an address is classified for egress. `Public` is the only class a policy allows by default: the
/// rest are the addresses that reach the machine itself, its private network or its cloud metadata
/// service, and a destination that resolves to one of them is refused unless the target's policy names
/// the class explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressClass {
    /// A globally routable address.
    Public,
    /// RFC 1918 (10/8, 172.16/12, 192.168/16) or RFC 4193 (`fc00::/7`).
    Private,
    /// 127/8 or `::1`.
    Loopback,
    /// 169.254/16 or `fe80::/10` — the cloud metadata service lives here.
    LinkLocal,
    /// 224/4 or `ff00::/8`.
    Multicast,
    /// 0.0.0.0 or `::`.
    Unspecified,
    /// 255.255.255.255.
    Broadcast,
    /// Reserved or documentation space that is never a legitimate egress destination.
    Reserved,
}

impl AddressClass {
    /// Canonical policy and log value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
            Self::Loopback => "loopback",
            Self::LinkLocal => "link_local",
            Self::Multicast => "multicast",
            Self::Unspecified => "unspecified",
            Self::Broadcast => "broadcast",
            Self::Reserved => "reserved",
        }
    }

    /// Parse the canonical policy value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "public" => Some(Self::Public),
            "private" => Some(Self::Private),
            "loopback" => Some(Self::Loopback),
            "link_local" => Some(Self::LinkLocal),
            "multicast" => Some(Self::Multicast),
            "unspecified" => Some(Self::Unspecified),
            "broadcast" => Some(Self::Broadcast),
            "reserved" => Some(Self::Reserved),
            _ => None,
        }
    }

    /// Classify an address.
    #[must_use]
    pub fn of(address: IpAddr) -> Self {
        match address {
            IpAddr::V4(v4) => Self::of_v4(v4),
            IpAddr::V6(v6) => Self::of_v6(v6),
        }
    }

    fn of_v4(address: Ipv4Addr) -> Self {
        let bits = u32::from(address);
        let in_range = |base: &str, prefix: u32| -> bool {
            let base: Ipv4Addr = base.parse().expect("literal");
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            bits & mask == u32::from(base) & mask
        };
        if address.is_unspecified() {
            Self::Unspecified
        } else if address.is_broadcast() {
            Self::Broadcast
        } else if address.is_loopback() {
            Self::Loopback
        } else if address.is_link_local() {
            Self::LinkLocal
        } else if address.is_private() {
            Self::Private
        } else if address.is_multicast() {
            Self::Multicast
        } else if in_range("100.64.0.0", 10)        // carrier-grade NAT
            || in_range("192.0.0.0", 24)             // IETF protocol assignments
            || in_range("192.0.2.0", 24)             // TEST-NET-1
            || in_range("198.18.0.0", 15)            // benchmarking
            || in_range("198.51.100.0", 24)          // TEST-NET-2
            || in_range("203.0.113.0", 24)           // TEST-NET-3
            || in_range("240.0.0.0", 4)
        {
            Self::Reserved
        } else {
            Self::Public
        }
    }

    fn of_v6(address: Ipv6Addr) -> Self {
        let bits = u128::from(address);
        let in_range = |base: &str, prefix: u32| -> bool {
            let base: Ipv6Addr = base.parse().expect("literal");
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            bits & mask == u128::from(base) & mask
        };
        // An IPv4-mapped address is the IPv4 address: classify it as such, so `::ffff:127.0.0.1`
        // cannot pass as a public IPv6 destination.
        if let Some(v4) = address.to_ipv4_mapped() {
            return Self::of_v4(v4);
        }
        if address.is_unspecified() {
            Self::Unspecified
        } else if address.is_loopback() {
            Self::Loopback
        } else if address.is_multicast() {
            Self::Multicast
        } else if in_range("fe80::", 10) {
            Self::LinkLocal
        } else if in_range("fc00::", 7) {
            Self::Private
        } else if in_range("2001:db8::", 32) || in_range("2001::", 32) || in_range("64:ff9b::", 96)
        {
            Self::Reserved
        } else {
            Self::Public
        }
    }

    /// The classes a policy allows when it names none.
    #[must_use]
    pub fn default_allowed() -> Vec<Self> {
        vec![Self::Public]
    }
}

impl fmt::Display for AddressClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A parsed egress destination: a scheme, a canonical lowercase host and a port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    scheme: String,
    host: String,
    port: u16,
    kind: HostKind,
}

impl Destination {
    /// Parse an absolute URL into a destination.
    ///
    /// The host is canonicalized — lowercased, and one trailing dot removed — because the grant lookup
    /// and the policy match are exact: `Example.COM.` and `example.com` are the same name and must
    /// therefore have the same answer, rather than one of them missing the grant.
    ///
    /// # Errors
    /// Returns [`EgressDenial::MalformedDestination`] for anything that is not an absolute `http`,
    /// `https`, `ws` or `wss` URL with an unambiguous host and no credentials in its authority.
    pub fn parse(url: &str) -> Result<Self, EgressDenial> {
        let (scheme, rest) = url.split_once("://").ok_or_else(|| {
            EgressDenial::MalformedDestination(format!("{url:?} is not an absolute URL"))
        })?;
        let scheme = scheme.to_ascii_lowercase();
        let default_port = match scheme.as_str() {
            "http" | "ws" => 80,
            "https" | "wss" => 443,
            other => {
                return Err(EgressDenial::MalformedDestination(format!(
                    "scheme {other:?} is not one the broker reaches"
                )))
            }
        };
        // The authority ends at the first path, query or fragment.
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if authority.is_empty() {
            return Err(EgressDenial::MalformedDestination(
                "the URL names no host".to_string(),
            ));
        }
        if authority.contains('@') {
            // Credentials in the authority would be carried into the request and could reach the log.
            return Err(EgressDenial::MalformedDestination(
                "credentials in the URL authority are not accepted".to_string(),
            ));
        }
        let (host, port) = split_authority(authority, default_port)?;
        let host = canonical_host(&host)?;
        let kind = if host.parse::<IpAddr>().is_ok() {
            HostKind::Literal
        } else {
            HostKind::Name
        };
        Ok(Self {
            scheme,
            host,
            port,
            kind,
        })
    }

    /// The canonical lowercase host.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// The scheme.
    #[must_use]
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// Whether the host was an IP literal.
    #[must_use]
    pub const fn kind(&self) -> HostKind {
        self.kind
    }
}

fn split_authority(authority: &str, default_port: u16) -> Result<(String, u16), EgressDenial> {
    if let Some(rest) = authority.strip_prefix('[') {
        // An IPv6 literal: `[::1]:8443`.
        let (host, tail) = rest.split_once(']').ok_or_else(|| {
            EgressDenial::MalformedDestination(
                "an IPv6 literal is missing its closing bracket".into(),
            )
        })?;
        let port = match tail {
            "" => default_port,
            other => parse_port(other.strip_prefix(':').ok_or_else(|| {
                EgressDenial::MalformedDestination(format!("{other:?} is not a port"))
            })?)?,
        };
        return Ok((host.to_string(), port));
    }
    match authority.split_once(':') {
        Some((host, port)) => Ok((host.to_string(), parse_port(port)?)),
        None => Ok((authority.to_string(), default_port)),
    }
}

fn parse_port(port: &str) -> Result<u16, EgressDenial> {
    port.parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(|| EgressDenial::MalformedDestination(format!("{port:?} is not a port number")))
}

fn canonical_host(host: &str) -> Result<String, EgressDenial> {
    // A trailing dot is the explicit root of an already-absolute name; it is the same destination.
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty() {
        return Err(EgressDenial::MalformedDestination(
            "the URL names no host".to_string(),
        ));
    }
    if host.contains(|c: char| c.is_whitespace() || c == '%' || c == '\\' || c == '\0') {
        return Err(EgressDenial::MalformedDestination(format!(
            "{host:?} is not a host"
        )));
    }
    Ok(host.to_ascii_lowercase())
}

/// Why an egress request was refused. Every variant names one rule; the default is the absence of a
/// grant, which is what "denied by default" means.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EgressDenial {
    /// The URL is not one the broker can reason about.
    #[error("malformed destination: {0}")]
    MalformedDestination(String),
    /// The host is one the broker never reaches, whatever any grant says.
    #[error("host {host:?} is never reachable: {why}")]
    BlockedHost {
        /// The host.
        host: String,
        /// Why it is always blocked.
        why: &'static str,
    },
    /// The target has no network policy, so nothing is permitted.
    #[error("execution target {target_id} has no network policy")]
    PolicyMissing {
        /// The target.
        target_id: String,
    },
    /// The target's policy denies egress outright.
    #[error("the network policy denies network.egress.new_destination")]
    PolicyDenies,
    /// The name resolved to addresses the policy does not allow.
    #[error("{host} resolves to {address}, which is {class}")]
    AddressNotAllowed {
        /// The host that resolved.
        host: String,
        /// The address it resolved to.
        address: IpAddr,
        /// That address's class.
        class: AddressClass,
    },
    /// The name resolved to nothing, so its addresses cannot be checked.
    #[error("{host} did not resolve, so its addresses are unknown")]
    Unresolved {
        /// The host that did not resolve.
        host: String,
    },
    /// No grant covers the destination: the deny-by-default case.
    #[error("no grant covers {host}:{port} for this target")]
    NoGrant {
        /// The host.
        host: String,
        /// The port.
        port: u16,
    },
    /// A grant covered the destination but has expired.
    #[error("the grant for {host}:{port} expired at {expires_at}")]
    GrantExpired {
        /// The host.
        host: String,
        /// The port.
        port: u16,
        /// When it expired.
        expires_at: String,
    },
    /// A grant covered the destination but was revoked.
    #[error("the grant for {host}:{port} was revoked at {revoked_at}")]
    GrantRevoked {
        /// The host.
        host: String,
        /// The port.
        port: u16,
        /// When it was revoked.
        revoked_at: String,
    },
    /// A grant covered the destination but a policy change fenced it.
    #[error(
        "the grant for {host}:{port} was issued under policy revision {issued}, which is now {current}"
    )]
    GrantFenced {
        /// The host.
        host: String,
        /// The port.
        port: u16,
        /// The revision the grant was issued under.
        issued: i32,
        /// The revision the policy is now at.
        current: i32,
    },
    /// A grant covered the destination but the target was rebound to another policy.
    #[error("the grant for {host}:{port} was issued under policy {from}, the target is now bound to {to}")]
    GrantRebound {
        /// The host.
        host: String,
        /// The port.
        port: u16,
        /// The policy the grant was issued under.
        from: String,
        /// The policy the target is bound to now.
        to: String,
    },
    /// A grant covered the destination but the target was replaced.
    #[error("the grant for {host}:{port} was issued under generation {issued}, the target is at {current}")]
    GrantGenerationStale {
        /// The host.
        host: String,
        /// The port.
        port: u16,
        /// The generation the grant was issued under.
        issued: i64,
        /// The target's generation now.
        current: i64,
    },
    /// A grant covered the destination but not for the capability asking.
    #[error("the grant for {host}:{port} is for capability {granted}, not {requested}")]
    GrantNotForCapability {
        /// The host.
        host: String,
        /// The port.
        port: u16,
        /// The capability the grant names.
        granted: String,
        /// The capability asking.
        requested: String,
    },
}

impl EgressDenial {
    /// The canonical rule name, for the decision log and for a test that needs to name the rule.
    #[must_use]
    pub const fn rule(&self) -> &'static str {
        match self {
            Self::MalformedDestination(_) => "malformed_destination",
            Self::BlockedHost { .. } => "blocked_host",
            Self::PolicyMissing { .. } => "policy_missing",
            Self::PolicyDenies => "policy_denies",
            Self::AddressNotAllowed { .. } => "address_not_allowed",
            Self::Unresolved { .. } => "unresolved",
            Self::NoGrant { .. } => "no_grant",
            Self::GrantExpired { .. } => "grant_expired",
            Self::GrantRevoked { .. } => "grant_revoked",
            Self::GrantFenced { .. } => "grant_fenced",
            Self::GrantRebound { .. } => "grant_rebound",
            Self::GrantGenerationStale { .. } => "grant_generation_stale",
            Self::GrantNotForCapability { .. } => "grant_not_for_capability",
        }
    }
}

/// Hosts the broker never reaches, whatever any grant or policy says. These are the names that resolve
/// to the machine itself or to a cloud metadata service, and a policy that could allow them would be a
/// policy that turns a grant into host access.
const BLOCKED_SUFFIXES: [&str; 5] = [
    ".localhost",
    ".local",
    ".internal",
    ".home.arpa",
    ".in-addr.arpa",
];
const BLOCKED_NAMES: [&str; 4] = [
    "localhost",
    "localhost.localdomain",
    "metadata.google.internal",
    "instance-data",
];

/// Whether a canonical host is one the broker always refuses, and why.
#[must_use]
pub fn blocked_host(host: &str) -> Option<&'static str> {
    if BLOCKED_NAMES.contains(&host) {
        return Some("it names the machine itself or a metadata service");
    }
    BLOCKED_SUFFIXES
        .iter()
        .find(|suffix| host.ends_with(*suffix))
        .map(|_| "it is a local or internal name")
}

/// The rules of a `policies` row that govern network egress (DOMAIN.md §7.3).
///
/// The network policy is the canonical Policy, not a second one: `execution_targets.network_policy_id`
/// already points at `policies`, so the broker reads the rule for `network.egress.new_destination` out
/// of that row's `rules` and takes `policies.version` as the revision it fences grants on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkPolicy {
    policy_id: String,
    version: i32,
    decision: PolicyRuleDecision,
    allowed_classes: Vec<AddressClass>,
}

/// What a network policy says about reaching a new destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyRuleDecision {
    /// The destination may be granted.
    Ask,
    /// The destination may be granted without asking.
    Allow,
    /// The destination is never granted.
    Deny,
}

impl PolicyRuleDecision {
    /// Canonical column value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }

    /// Parse the canonical column value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ask" => Some(Self::Ask),
            "allow" => Some(Self::Allow),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }
}

/// The effect class whose rule is the network policy (DOMAIN.md §7.1).
pub const NETWORK_EFFECT_CLASS: &str = "network.egress.new_destination";

impl NetworkPolicy {
    /// A policy with no network rule: it permits nothing.
    #[must_use]
    pub fn denying(policy_id: impl Into<String>, version: i32) -> Self {
        Self {
            policy_id: policy_id.into(),
            version,
            decision: PolicyRuleDecision::Deny,
            allowed_classes: Vec::new(),
        }
    }

    /// Read the network rule out of a `policies.rules` array.
    ///
    /// # Errors
    /// Returns [`EgressError::PolicyRuleInvalid`] when a rule for the network effect class is present
    /// but names a decision or an address class the domain does not define. A rule that cannot be
    /// understood is refused rather than defaulted: a policy whose meaning is guessed is not a policy.
    pub fn from_rules(
        policy_id: &str,
        version: i32,
        rules: &serde_json::Value,
    ) -> Result<Self, EgressError> {
        let rules = rules
            .as_array()
            .ok_or_else(|| EgressError::PolicyRuleInvalid {
                detail: "rules is not an array".to_string(),
            })?;
        let Some(rule) = rules.iter().find(|rule| {
            rule.get("effect_class").and_then(serde_json::Value::as_str)
                == Some(NETWORK_EFFECT_CLASS)
        }) else {
            return Ok(Self::denying(policy_id, version));
        };
        let decision = rule
            .get("decision")
            .and_then(serde_json::Value::as_str)
            .and_then(PolicyRuleDecision::parse)
            .ok_or_else(|| EgressError::PolicyRuleInvalid {
                detail: "the network rule has no decision the domain defines".to_string(),
            })?;
        let allowed_classes = match rule
            .get("conditions")
            .and_then(|conditions| conditions.get("allowed_address_classes"))
        {
            None => AddressClass::default_allowed(),
            Some(value) => {
                let listed = value
                    .as_array()
                    .ok_or_else(|| EgressError::PolicyRuleInvalid {
                        detail: "allowed_address_classes is not an array".to_string(),
                    })?;
                listed
                    .iter()
                    .map(|entry| {
                        entry.as_str().and_then(AddressClass::parse).ok_or_else(|| {
                            EgressError::PolicyRuleInvalid {
                                detail: format!("{entry} is not an address class"),
                            }
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?
            }
        };
        Ok(Self {
            policy_id: policy_id.to_string(),
            version,
            decision,
            allowed_classes,
        })
    }

    /// The policy's identity.
    #[must_use]
    pub fn policy_id(&self) -> &str {
        &self.policy_id
    }

    /// The revision, which is what grants are fenced on.
    #[must_use]
    pub const fn version(&self) -> i32 {
        self.version
    }

    /// What the policy says about reaching a new destination.
    #[must_use]
    pub const fn decision(&self) -> PolicyRuleDecision {
        self.decision
    }

    /// The address classes the policy allows.
    #[must_use]
    pub fn allowed_classes(&self) -> &[AddressClass] {
        &self.allowed_classes
    }

    /// Whether the policy allows `class`.
    #[must_use]
    pub fn allows(&self, class: AddressClass) -> bool {
        self.allowed_classes.contains(&class)
    }
}

/// One request to reach a destination.
#[derive(Debug)]
pub struct EgressRequest<'a> {
    /// The tenant the target belongs to.
    pub tenant_id: &'a str,
    /// The execution target making the request.
    pub target_id: &'a str,
    /// The target's generation; a grant issued under another generation is not this target's.
    pub target_generation: i64,
    /// The capability the call runs under, when it is narrowed to one.
    pub capability_id: Option<&'a str>,
    /// Who is asking.
    pub origin: EgressOrigin,
    /// The absolute URL being reached.
    pub url: &'a str,
    /// Every address the host resolved to, as the resolver returned them.
    pub resolved_addresses: &'a [IpAddr],
    /// The instant of the decision.
    pub at: &'a str,
    /// The headers the call would carry. These are deliberately *not* consulted: they are here so a
    /// test can prove no header value reaches the decision log.
    pub headers: &'a [(&'a str, &'a str)],
    /// An opaque credential handle the call would present, never its material (EXEC-007).
    pub credential_ref: Option<&'a str>,
}

/// The broker's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// A live grant covers the destination.
    Allow {
        /// The effect that authorized the destination.
        effect_id: String,
    },
    /// Nothing permits the destination.
    Deny(EgressDenial),
}

impl Decision {
    /// Whether the destination may be reached.
    #[must_use]
    pub const fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow { .. })
    }

    /// The canonical rule name the log records.
    #[must_use]
    pub fn rule(&self) -> &'static str {
        match self {
            Self::Allow { .. } => "allow",
            Self::Deny(denial) => denial.rule(),
        }
    }
}

/// What the broker records about a decision. It is built from the destination, the decision and
/// identifiers, so there is nowhere for a header value or a credential material to be recorded; the
/// rendering is a single log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRecord {
    /// The tenant.
    pub tenant_id: String,
    /// The target.
    pub target_id: String,
    /// Who asked.
    pub origin: EgressOrigin,
    /// The destination's canonical host.
    pub host: String,
    /// The destination's port.
    pub port: u16,
    /// The capability the call ran under.
    pub capability_id: Option<String>,
    /// The opaque credential handle, which is an identifier rather than a material.
    pub credential_ref: Option<String>,
    /// The rule that decided.
    pub rule: &'static str,
    /// The effect that authorized it, when one did.
    pub grant_effect_id: Option<String>,
    /// The instant.
    pub at: String,
}

impl fmt::Display for DecisionRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "egress tenant={} target={} origin={} destination={}:{} capability={} credential={} decision={} grant={} at={}",
            self.tenant_id,
            self.target_id,
            self.origin.as_str(),
            self.host,
            self.port,
            self.capability_id.as_deref().unwrap_or("-"),
            self.credential_ref.as_deref().unwrap_or("-"),
            self.rule,
            self.grant_effect_id.as_deref().unwrap_or("-"),
            self.at,
        )
    }
}

/// The broker's rules, as pure functions over a policy and the target's grants.
pub struct EgressBroker;

impl EgressBroker {
    /// Decide whether a request may reach its destination.
    ///
    /// The order is deliberate and fail-closed: the destination must be understood, the host must not
    /// be one the broker never reaches, the policy must permit the class of *every* address the name
    /// resolved to, and only then is a live grant looked for. An unknown destination therefore has no
    /// path to `Allow`.
    ///
    /// # Errors
    /// Returns [`EgressDenial::MalformedDestination`] when the URL cannot be parsed: the caller gets a
    /// `Deny` either way, but a caller that must not proceed at all needs to know the request was not
    /// even a destination.
    pub fn decide(
        request: &EgressRequest<'_>,
        policy: &NetworkPolicy,
        grants: &[EgressGrant],
    ) -> Result<Decision, EgressDenial> {
        let destination = Destination::parse(request.url)?;

        if let Some(why) = blocked_host(destination.host()) {
            return Ok(Decision::Deny(EgressDenial::BlockedHost {
                host: destination.host().to_string(),
                why,
            }));
        }

        if policy.decision() == PolicyRuleDecision::Deny {
            return Ok(Decision::Deny(EgressDenial::PolicyDenies));
        }

        if request.resolved_addresses.is_empty() {
            return Ok(Decision::Deny(EgressDenial::Unresolved {
                host: destination.host().to_string(),
            }));
        }
        for address in request.resolved_addresses {
            let class = AddressClass::of(*address);
            if !policy.allows(class) {
                return Ok(Decision::Deny(EgressDenial::AddressNotAllowed {
                    host: destination.host().to_string(),
                    address: *address,
                    class,
                }));
            }
        }

        match Self::covering_grant(request, &destination, policy, grants) {
            GrantLookup::Live { effect_id } => Ok(Decision::Allow { effect_id }),
            GrantLookup::Absent => Ok(Decision::Deny(EgressDenial::NoGrant {
                host: destination.host().to_string(),
                port: destination.port(),
            })),
            GrantLookup::Refused(denial) => Ok(Decision::Deny(denial)),
        }
    }

    /// Build the log line for a decision.
    #[must_use]
    pub fn record(request: &EgressRequest<'_>, decision: &Decision, at: &str) -> DecisionRecord {
        let (host, port) = Destination::parse(request.url)
            .map(|destination| (destination.host().to_string(), destination.port()))
            .unwrap_or_else(|_| (request.url.to_string(), 0));
        DecisionRecord {
            tenant_id: request.tenant_id.to_string(),
            target_id: request.target_id.to_string(),
            origin: request.origin,
            host,
            port,
            capability_id: request.capability_id.map(str::to_string),
            credential_ref: request.credential_ref.map(str::to_string),
            rule: decision.rule(),
            grant_effect_id: match decision {
                Decision::Allow { effect_id } => Some(effect_id.clone()),
                Decision::Deny(_) => None,
            },
            at: at.to_string(),
        }
    }

    /// Find the grant that covers the destination, or the most specific reason none does.
    ///
    /// A live grant is looked for first and wins outright: an explanation is only owed when the answer
    /// is no, so a fenced or stale grant that happens to sit in the same list must never override one
    /// that still entitles the caller. Only once nothing is live is the reason chosen, most specific
    /// first, so a caller learns *why* rather than only that it was refused.
    fn covering_grant(
        request: &EgressRequest<'_>,
        destination: &Destination,
        policy: &NetworkPolicy,
        grants: &[EgressGrant],
    ) -> GrantLookup {
        let for_host: Vec<&EgressGrant> = grants
            .iter()
            .filter(|grant| {
                grant.tenant_id == request.tenant_id
                    && grant.target_id == request.target_id
                    && grant.host == destination.host()
                    && grant.port == destination.port()
            })
            .collect();

        let mut live: Vec<&EgressGrant> = for_host
            .iter()
            .copied()
            .filter(|grant| {
                grant.revoked_at.is_none()
                    && grant.expires_at.as_str() > request.at
                    && grant.policy_id == policy.policy_id()
                    && grant.policy_version == policy.version()
                    && grant.target_generation == request.target_generation
                    && match (&grant.capability_id, request.capability_id) {
                        (None, _) => true,
                        (Some(granted), Some(requested)) => granted == requested,
                        (Some(_), None) => false,
                    }
            })
            .collect();
        // Deterministic: the grant that lasts longest, then by identity.
        live.sort_by(|left, right| {
            right
                .expires_at
                .cmp(&left.expires_at)
                .then_with(|| left.effect_id.cmp(&right.effect_id))
        });
        if let Some(grant) = live.first() {
            return GrantLookup::Live {
                effect_id: grant.effect_id.clone(),
            };
        }

        // Nothing is live. Explain it from the most recently issued grant for the destination, which is
        // the one the caller most likely means; a fixed rule order would instead report whichever
        // stale grant happens to sort first, which is not the rule the caller can act on.
        let mut candidates: Vec<&EgressGrant> = for_host.clone();
        candidates.sort_by(|left, right| {
            right
                .issued_at
                .cmp(&left.issued_at)
                .then_with(|| left.effect_id.cmp(&right.effect_id))
        });
        match candidates
            .first()
            .and_then(|grant| refusal(grant, request, policy, destination))
        {
            Some(denial) => GrantLookup::Refused(denial),
            None => GrantLookup::Absent,
        }
    }
}

/// The rule a grant that is not live violates, if it violates one.
///
/// The order inside a single grant is fixed and most-fundamental first: a grant for another policy or
/// another revision was never this target's, one for another generation was for another machine, one
/// for another capability was for another caller, and only then is it a matter of revocation or time.
fn refusal(
    grant: &EgressGrant,
    request: &EgressRequest<'_>,
    policy: &NetworkPolicy,
    destination: &Destination,
) -> Option<EgressDenial> {
    let host = destination.host().to_string();
    let port = destination.port();
    if grant.policy_id != policy.policy_id() {
        return Some(EgressDenial::GrantRebound {
            host,
            port,
            from: grant.policy_id.clone(),
            to: policy.policy_id().to_string(),
        });
    }
    if grant.policy_version != policy.version() {
        return Some(EgressDenial::GrantFenced {
            host,
            port,
            issued: grant.policy_version,
            current: policy.version(),
        });
    }
    if grant.target_generation != request.target_generation {
        return Some(EgressDenial::GrantGenerationStale {
            host,
            port,
            issued: grant.target_generation,
            current: request.target_generation,
        });
    }
    if let (Some(granted), Some(requested)) = (&grant.capability_id, request.capability_id) {
        if granted != requested {
            return Some(EgressDenial::GrantNotForCapability {
                host,
                port,
                granted: granted.clone(),
                requested: requested.to_string(),
            });
        }
    }
    if grant.capability_id.is_some() && request.capability_id.is_none() {
        return Some(EgressDenial::GrantNotForCapability {
            host,
            port,
            granted: grant.capability_id.clone().unwrap_or_default(),
            requested: String::new(),
        });
    }
    if let Some(revoked_at) = &grant.revoked_at {
        return Some(EgressDenial::GrantRevoked {
            host,
            port,
            revoked_at: revoked_at.clone(),
        });
    }
    if grant.expires_at.as_str() <= request.at {
        return Some(EgressDenial::GrantExpired {
            host,
            port,
            expires_at: grant.expires_at.clone(),
        });
    }
    None
}

enum GrantLookup {
    Live { effect_id: String },
    Refused(EgressDenial),
    Absent,
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-09-13T10:00:00Z";
    const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
    const TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";
    const POLICY: &str = "pol_01J8Z3K6F1N8VQ2X5W9Y0FFFFF";

    /// A globally routable address, as a slice that outlives every request built from it.
    fn public() -> &'static [IpAddr] {
        static PUBLIC: &[IpAddr] = &[IpAddr::V4(std::net::Ipv4Addr::new(93, 184, 216, 34))];
        PUBLIC
    }

    fn policy(decision: PolicyRuleDecision, classes: &[AddressClass]) -> NetworkPolicy {
        NetworkPolicy::from_rules(
            POLICY,
            3,
            &serde_json::json!([{
                "effect_class": NETWORK_EFFECT_CLASS,
                "decision": decision.as_str(),
                "conditions": {
                    "allowed_address_classes": classes.iter().map(|c| c.as_str()).collect::<Vec<_>>()
                }
            }]),
        )
        .expect("a well-formed policy")
    }

    fn grant(host: &str, port: u16) -> EgressGrant {
        EgressGrant {
            effect_id: "eff_01J8Z3K6F1N8VQ2X5W9Y0FFFFF".to_string(),
            tenant_id: TENANT.to_string(),
            target_id: TARGET.to_string(),
            target_generation: 1,
            capability_id: None,
            policy_id: POLICY.to_string(),
            policy_version: 3,
            host: host.to_string(),
            port,
            issued_at: "2026-09-13T09:00:00Z".to_string(),
            expires_at: "2026-09-13T11:00:00Z".to_string(),
            revoked_at: None,
        }
    }

    fn request<'a>(url: &'a str, addresses: &'a [IpAddr]) -> EgressRequest<'a> {
        EgressRequest {
            tenant_id: TENANT,
            target_id: TARGET,
            target_generation: 1,
            capability_id: None,
            origin: EgressOrigin::Tool,
            url,
            resolved_addresses: addresses,
            at: NOW,
            headers: &[],
            credential_ref: None,
        }
    }

    // ------------------------------------------------------------------ destinations

    #[test]
    fn a_destination_is_canonicalized_so_one_name_has_one_answer() {
        let canonical = Destination::parse("https://Example.COM./v1/things?q=1").expect("parses");
        assert_eq!(canonical.host(), "example.com");
        assert_eq!(canonical.port(), 443);
        assert_eq!(canonical.kind(), HostKind::Name);
        assert_eq!(canonical.scheme(), "https");

        // The same name, every way it can be written, is the same destination.
        for url in [
            "https://example.com",
            "https://EXAMPLE.com/",
            "https://example.com.?a=b",
            "https://example.com.",
        ] {
            assert_eq!(
                Destination::parse(url).expect("parses").host(),
                "example.com",
                "{url}"
            );
        }
    }

    #[test]
    fn a_destination_carries_its_port_and_scheme() {
        assert_eq!(
            Destination::parse("http://example.com")
                .expect("parses")
                .port(),
            80
        );
        assert_eq!(
            Destination::parse("https://example.com:8443/x")
                .expect("parses")
                .port(),
            8443
        );
        let v6 = Destination::parse("https://[2606:2800:220:1:248:1893:25c8:1946]:8443/x")
            .expect("parses");
        assert_eq!(v6.host(), "2606:2800:220:1:248:1893:25c8:1946");
        assert_eq!(v6.port(), 8443);
        assert_eq!(v6.kind(), HostKind::Literal);
    }

    #[test]
    fn a_request_that_is_not_a_destination_is_refused() {
        for url in [
            "example.com",
            "ftp://example.com",
            "file:///etc/passwd",
            "https://user:secret@example.com",
            "https://",
            "https://example.com:0",
            "https://example.com:notaport",
            "https://[::1",
        ] {
            assert!(
                matches!(
                    Destination::parse(url),
                    Err(EgressDenial::MalformedDestination(_))
                ),
                "{url} must not parse"
            );
        }
    }

    #[test]
    fn hosts_the_broker_never_reaches_are_named() {
        for host in [
            "localhost",
            "localhost.localdomain",
            "metadata.google.internal",
            "instance-data",
            "printer.local",
            "vault.internal",
            "box.home.arpa",
        ] {
            assert!(blocked_host(host).is_some(), "{host} must be blocked");
        }
        for host in ["example.com", "api.github.com", "localhost.example.com"] {
            assert!(blocked_host(host).is_none(), "{host} must not be blocked");
        }
    }

    // ------------------------------------------------------------------ address classes

    #[test]
    fn addresses_are_classified_by_what_they_reach() {
        let cases: [(&str, AddressClass); 22] = [
            ("93.184.216.34", AddressClass::Public),
            ("8.8.8.8", AddressClass::Public),
            ("127.0.0.1", AddressClass::Loopback),
            ("10.1.2.3", AddressClass::Private),
            ("172.16.5.4", AddressClass::Private),
            ("192.168.1.1", AddressClass::Private),
            ("169.254.169.254", AddressClass::LinkLocal),
            ("0.0.0.0", AddressClass::Unspecified),
            ("255.255.255.255", AddressClass::Broadcast),
            ("224.0.0.1", AddressClass::Multicast),
            ("100.64.0.1", AddressClass::Reserved),
            ("192.0.2.10", AddressClass::Reserved),
            ("198.18.0.1", AddressClass::Reserved),
            ("240.0.0.1", AddressClass::Reserved),
            ("2606:2800:220:1:248:1893:25c8:1946", AddressClass::Public),
            ("::1", AddressClass::Loopback),
            ("fe80::1", AddressClass::LinkLocal),
            ("fd00::1", AddressClass::Private),
            ("::", AddressClass::Unspecified),
            ("ff02::1", AddressClass::Multicast),
            ("2001:db8::1", AddressClass::Reserved),
            // An IPv4-mapped address is the IPv4 address, so it cannot pass as public IPv6.
            ("::ffff:127.0.0.1", AddressClass::Loopback),
        ];
        for (address, expected) in cases {
            let address: IpAddr = address.parse().expect("literal");
            assert_eq!(AddressClass::of(address), expected, "{address}");
        }
    }

    // ------------------------------------------------------------------ the decision

    #[test]
    fn an_unknown_destination_is_denied_by_default() {
        let decision = EgressBroker::decide(
            &request("https://nothing-granted.example/path", public()),
            &policy(PolicyRuleDecision::Ask, &[AddressClass::Public]),
            &[],
        )
        .expect("understood");
        assert_eq!(
            decision,
            Decision::Deny(EgressDenial::NoGrant {
                host: "nothing-granted.example".to_string(),
                port: 443,
            })
        );
        assert_eq!(decision.rule(), "no_grant");
    }

    #[test]
    fn a_granted_destination_with_an_allowed_address_is_reached() {
        let decision = EgressBroker::decide(
            &request("https://api.example.com/v1", public()),
            &policy(PolicyRuleDecision::Ask, &[AddressClass::Public]),
            &[grant("api.example.com", 443)],
        )
        .expect("understood");
        assert!(decision.is_allowed(), "got {decision:?}");
        assert_eq!(
            decision,
            Decision::Allow {
                effect_id: "eff_01J8Z3K6F1N8VQ2X5W9Y0FFFFF".to_string()
            }
        );
    }

    #[test]
    fn a_granted_name_is_still_refused_when_it_resolves_inside() {
        // The DNS rebinding case: the grant is for the name, and the name now points at the machine
        // itself. The grant cannot make the address acceptable.
        for (address, class) in [
            ("127.0.0.1", AddressClass::Loopback),
            ("10.0.0.7", AddressClass::Private),
            ("169.254.169.254", AddressClass::LinkLocal),
            ("::ffff:127.0.0.1", AddressClass::Loopback),
        ] {
            let address: IpAddr = address.parse().expect("literal");
            let decision = EgressBroker::decide(
                &request("https://api.example.com/v1", &[address]),
                &policy(PolicyRuleDecision::Ask, &[AddressClass::Public]),
                &[grant("api.example.com", 443)],
            )
            .expect("understood");
            assert_eq!(
                decision,
                Decision::Deny(EgressDenial::AddressNotAllowed {
                    host: "api.example.com".to_string(),
                    address,
                    class,
                }),
                "{address}"
            );
        }

        // A name that resolves to both a public and an internal address is refused: one internal
        // answer is enough.
        let mixed: Vec<IpAddr> = vec![
            "93.184.216.34".parse().expect("literal"),
            "127.0.0.1".parse().expect("literal"),
        ];
        let decision = EgressBroker::decide(
            &request("https://api.example.com/", &mixed),
            &policy(PolicyRuleDecision::Ask, &[AddressClass::Public]),
            &[grant("api.example.com", 443)],
        )
        .expect("understood");
        assert!(matches!(
            decision,
            Decision::Deny(EgressDenial::AddressNotAllowed { .. })
        ));
    }

    #[test]
    fn an_address_the_policy_names_is_reachable_when_the_caller_asked_for_it() {
        // A self-hosted connector on a private network is a deliberate policy choice, and then it
        // works: the class check is a rule, not a blanket ban.
        let private: Vec<IpAddr> = vec!["10.0.0.7".parse().expect("literal")];
        let decision = EgressBroker::decide(
            &request("https://connector.internal.example/", &private),
            &policy(
                PolicyRuleDecision::Ask,
                &[AddressClass::Public, AddressClass::Private],
            ),
            &[grant("connector.internal.example", 443)],
        )
        .expect("understood");
        assert!(decision.is_allowed(), "got {decision:?}");
    }

    #[test]
    fn a_name_that_did_not_resolve_is_refused_rather_than_assumed_fine() {
        let decision = EgressBroker::decide(
            &request("https://api.example.com/", &[]),
            &policy(PolicyRuleDecision::Ask, &[AddressClass::Public]),
            &[grant("api.example.com", 443)],
        )
        .expect("understood");
        assert_eq!(
            decision,
            Decision::Deny(EgressDenial::Unresolved {
                host: "api.example.com".to_string()
            })
        );
    }

    #[test]
    fn a_policy_that_denies_stops_the_destination_before_any_grant_is_consulted() {
        let decision = EgressBroker::decide(
            &request("https://api.example.com/", public()),
            &policy(PolicyRuleDecision::Deny, &[AddressClass::Public]),
            &[grant("api.example.com", 443)],
        )
        .expect("understood");
        assert_eq!(decision, Decision::Deny(EgressDenial::PolicyDenies));
    }

    #[test]
    fn a_policy_with_no_network_rule_permits_nothing() {
        let none = NetworkPolicy::from_rules(POLICY, 1, &serde_json::json!([])).expect("parses");
        assert_eq!(none.decision(), PolicyRuleDecision::Deny);
        assert!(none.allowed_classes().is_empty());
        let decision = EgressBroker::decide(
            &request("https://api.example.com/", public()),
            &none,
            &[grant("api.example.com", 443)],
        )
        .expect("understood");
        assert_eq!(decision, Decision::Deny(EgressDenial::PolicyDenies));
    }

    #[test]
    fn a_rule_the_domain_does_not_define_is_refused_rather_than_defaulted() {
        let bad_decision = serde_json::json!([{
            "effect_class": NETWORK_EFFECT_CLASS,
            "decision": "probably",
        }]);
        assert!(matches!(
            NetworkPolicy::from_rules(POLICY, 1, &bad_decision),
            Err(EgressError::PolicyRuleInvalid { .. })
        ));
        let bad_class = serde_json::json!([{
            "effect_class": NETWORK_EFFECT_CLASS,
            "decision": "ask",
            "conditions": { "allowed_address_classes": ["everywhere"] },
        }]);
        assert!(matches!(
            NetworkPolicy::from_rules(POLICY, 1, &bad_class),
            Err(EgressError::PolicyRuleInvalid { .. })
        ));
        // A rule for a class the broker does not govern is not its business, so it is ignored.
        let other = serde_json::json!([{ "effect_class": "read.internal", "decision": "allow" }]);
        let policy = NetworkPolicy::from_rules(POLICY, 1, &other).expect("parses");
        assert_eq!(policy.decision(), PolicyRuleDecision::Deny);
    }

    // ------------------------------------------------------- what retires a grant

    #[test]
    fn a_policy_change_fences_every_grant_at_the_previous_revision() {
        let grants = [grant("api.example.com", 443)];
        let before = EgressBroker::decide(
            &request("https://api.example.com/", public()),
            &policy(PolicyRuleDecision::Ask, &[AddressClass::Public]),
            &grants,
        )
        .expect("understood");
        assert!(before.is_allowed());

        // The policy owner tightens the policy, which increments `policies.version`. No grant is
        // rewritten and no scan happens: the fence is the comparison.
        let tightened = NetworkPolicy::from_rules(
            POLICY,
            4,
            &serde_json::json!([{
                "effect_class": NETWORK_EFFECT_CLASS,
                "decision": "ask",
                "conditions": { "allowed_address_classes": ["public"] }
            }]),
        )
        .expect("parses");
        let after = EgressBroker::decide(
            &request("https://api.example.com/", public()),
            &tightened,
            &grants,
        )
        .expect("understood");
        assert_eq!(
            after,
            Decision::Deny(EgressDenial::GrantFenced {
                host: "api.example.com".to_string(),
                port: 443,
                issued: 3,
                current: 4,
            })
        );
    }

    #[test]
    fn an_expired_or_revoked_grant_is_refused_with_its_own_reason() {
        let mut expired = grant("api.example.com", 443);
        expired.expires_at = "2026-09-13T09:59:59Z".to_string();
        let decision = EgressBroker::decide(
            &request("https://api.example.com/", public()),
            &policy(PolicyRuleDecision::Ask, &[AddressClass::Public]),
            &[expired],
        )
        .expect("understood");
        assert_eq!(
            decision,
            Decision::Deny(EgressDenial::GrantExpired {
                host: "api.example.com".to_string(),
                port: 443,
                expires_at: "2026-09-13T09:59:59Z".to_string(),
            })
        );

        // Expiry is exclusive: a grant is not usable at the instant it expires.
        let mut boundary = grant("api.example.com", 443);
        boundary.expires_at = NOW.to_string();
        let decision = EgressBroker::decide(
            &request("https://api.example.com/", public()),
            &policy(PolicyRuleDecision::Ask, &[AddressClass::Public]),
            &[boundary],
        )
        .expect("understood");
        assert!(matches!(
            decision,
            Decision::Deny(EgressDenial::GrantExpired { .. })
        ));

        let mut revoked = grant("api.example.com", 443);
        revoked.revoked_at = Some("2026-09-13T09:30:00Z".to_string());
        let decision = EgressBroker::decide(
            &request("https://api.example.com/", public()),
            &policy(PolicyRuleDecision::Ask, &[AddressClass::Public]),
            &[revoked],
        )
        .expect("understood");
        assert_eq!(
            decision,
            Decision::Deny(EgressDenial::GrantRevoked {
                host: "api.example.com".to_string(),
                port: 443,
                revoked_at: "2026-09-13T09:30:00Z".to_string(),
            })
        );
    }

    #[test]
    fn a_grant_belongs_to_one_target_generation_and_one_capability() {
        let mut replaced = grant("api.example.com", 443);
        replaced.target_generation = 1;
        let mut later = request("https://api.example.com/", public());
        later.target_generation = 2;
        let decision = EgressBroker::decide(
            &later,
            &policy(PolicyRuleDecision::Ask, &[AddressClass::Public]),
            &[replaced.clone()],
        )
        .expect("understood");
        assert_eq!(
            decision,
            Decision::Deny(EgressDenial::GrantGenerationStale {
                host: "api.example.com".to_string(),
                port: 443,
                issued: 1,
                current: 2,
            })
        );

        let mut narrowed = grant("api.example.com", 443);
        narrowed.capability_id = Some("cap_01J8Z3K6F1N8VQ2X5W9Y0FFFFF".to_string());
        let mut asking = request("https://api.example.com/", public());
        asking.capability_id = Some("cap_01J8Z3K6F1N8VQ2X5W9Y0GGGGG");
        let decision = EgressBroker::decide(
            &asking,
            &policy(PolicyRuleDecision::Ask, &[AddressClass::Public]),
            &[narrowed.clone()],
        )
        .expect("understood");
        assert!(matches!(
            decision,
            Decision::Deny(EgressDenial::GrantNotForCapability { .. })
        ));

        // The capability the grant names is the one that may use it.
        let mut asking = request("https://api.example.com/", public());
        asking.capability_id = Some("cap_01J8Z3K6F1N8VQ2X5W9Y0FFFFF");
        assert!(EgressBroker::decide(
            &asking,
            &policy(PolicyRuleDecision::Ask, &[AddressClass::Public]),
            &[narrowed]
        )
        .expect("understood")
        .is_allowed());
    }

    #[test]
    fn a_grant_for_another_port_or_host_does_not_cover_the_destination() {
        let grants = [grant("api.example.com", 8443)];
        let decision = EgressBroker::decide(
            &request("https://api.example.com/", public()),
            &policy(PolicyRuleDecision::Ask, &[AddressClass::Public]),
            &grants,
        )
        .expect("understood");
        assert!(matches!(
            decision,
            Decision::Deny(EgressDenial::NoGrant { .. })
        ));

        let decision = EgressBroker::decide(
            &request("https://other.example.com/", public()),
            &policy(PolicyRuleDecision::Ask, &[AddressClass::Public]),
            &[
                grant("other.example.com", 443),
                grant("api.example.com", 443),
            ],
        )
        .expect("understood");
        assert!(decision.is_allowed());
    }

    #[test]
    fn the_longest_lived_live_grant_is_the_one_reported() {
        let mut first = grant("api.example.com", 443);
        first.effect_id = "eff_01J8Z3K6F1N8VQ2X5W9Y0AAAAA".to_string();
        first.expires_at = "2026-09-13T10:30:00Z".to_string();
        let mut second = grant("api.example.com", 443);
        second.effect_id = "eff_01J8Z3K6F1N8VQ2X5W9Y0BBBBB".to_string();
        second.expires_at = "2026-09-13T12:00:00Z".to_string();
        // A stale-generation grant with the longest life does not win: it is not live at all.
        let mut stale = grant("api.example.com", 443);
        stale.effect_id = "eff_01J8Z3K6F1N8VQ2X5W9Y0CCCCC".to_string();
        stale.target_generation = 9;
        stale.expires_at = "2026-09-14T00:00:00Z".to_string();

        let decision = EgressBroker::decide(
            &request("https://api.example.com/", public()),
            &policy(PolicyRuleDecision::Ask, &[AddressClass::Public]),
            &[second, first, stale],
        )
        .expect("understood");
        assert_eq!(
            decision,
            Decision::Allow {
                effect_id: "eff_01J8Z3K6F1N8VQ2X5W9Y0BBBBB".to_string()
            }
        );
    }

    #[test]
    fn no_grant_from_another_tenant_or_target_is_ever_considered() {
        let mut elsewhere = grant("api.example.com", 443);
        elsewhere.tenant_id = "tn_01J8Z3K6F1N8VQ2X5W9Y0GGGGG".to_string();
        let decision = EgressBroker::decide(
            &request("https://api.example.com/", public()),
            &policy(PolicyRuleDecision::Ask, &[AddressClass::Public]),
            &[elsewhere],
        )
        .expect("understood");
        assert!(matches!(
            decision,
            Decision::Deny(EgressDenial::NoGrant { .. })
        ));
    }

    // ------------------------------------------------------- the log

    #[test]
    fn the_decision_log_records_the_destination_and_never_a_header_value() {
        const CANARY: &str = "canary-bearer-value-9f3a";
        let headers = [
            ("authorization", format!("Bearer {CANARY}")),
            ("cookie", "session=also-secret".to_string()),
        ];
        let borrows: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let request = EgressRequest {
            headers: &borrows,
            credential_ref: Some("sec_01J8Z3K6F1N8VQ2X5W9Y0FFFFF"),
            ..request("https://api.example.com/v1/things", public())
        };

        let decision = EgressBroker::decide(
            &request,
            &policy(PolicyRuleDecision::Ask, &[AddressClass::Public]),
            &[grant("api.example.com", 443)],
        )
        .expect("understood");
        let line = EgressBroker::record(&request, &decision, NOW).to_string();

        assert!(
            line.contains("destination=api.example.com:443"),
            "the log must say what was reached: {line}"
        );
        assert!(line.contains("decision=allow"), "{line}");
        assert!(
            line.contains("credential=sec_01J8Z3K6F1N8VQ2X5W9Y0FFFFF"),
            "the handle is an identifier and belongs in the log: {line}"
        );
        for secret in [CANARY, "also-secret"] {
            assert!(
                !line.contains(secret),
                "a header value reached the log: {line}"
            );
        }

        // A refused request logs too, and says which rule refused it.
        let denied = EgressBroker::decide(
            &request,
            &policy(PolicyRuleDecision::Ask, &[AddressClass::Public]),
            &[],
        )
        .expect("understood");
        let line = EgressBroker::record(&request, &denied, NOW).to_string();
        assert!(line.contains("decision=no_grant"), "{line}");
        assert!(!line.contains(CANARY), "{line}");
    }

    #[test]
    fn the_same_destination_reaches_the_same_answer_from_every_caller() {
        let grants = [grant("api.example.com", 443)];
        let policy = policy(PolicyRuleDecision::Ask, &[AddressClass::Public]);
        let answers: Vec<Decision> = [
            EgressOrigin::Tool,
            EgressOrigin::Browser,
            EgressOrigin::Connector,
            EgressOrigin::Webhook,
        ]
        .into_iter()
        .map(|origin| {
            let request = EgressRequest {
                origin,
                ..request("https://api.example.com/", public())
            };
            EgressBroker::decide(&request, &policy, &grants).expect("understood")
        })
        .collect();
        assert!(
            answers.iter().all(|answer| answer.is_allowed()),
            "each caller must be governed by the same rules: {answers:?}"
        );
        assert!(answers.windows(2).all(|pair| pair[0] == pair[1]));
    }
}
