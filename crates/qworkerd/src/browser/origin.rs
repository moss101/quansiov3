//! Origins and the navigation allow-list (ADP-001).
//!
//! Two decisions in the hardened browser depend on reading a URL the same way every time: whether a
//! destination may be navigated to at all, and whether the page a secret is about to be typed into is
//! one the secret was issued for. Both live here so they cannot disagree.
//!
//! The parser is deliberately small and strict rather than general. It understands `scheme://authority`
//! and nothing else, refuses userinfo (`user:pass@host`) outright, lowercases the scheme and host, and
//! drops a port that is the scheme's default. Anything it does not recognise has no origin, and a URL
//! with no origin is never a match for anything.

use super::fault::{BrowserFault, FaultCode};

/// The origin of a `http` or `https` URL (`scheme://host[:port]`), or `None` when the URL has no
/// tuple origin (`about:blank`, `data:`, a URL with credentials, a malformed one).
#[must_use]
pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    // Userinfo is a credential in a URL. It is not an origin, and it is never accepted as one.
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let authority = authority.to_ascii_lowercase();
    let (host, port) = split_host_port(&authority)?;
    if host.is_empty() {
        return None;
    }
    let default_port = if scheme == "https" { "443" } else { "80" };
    match port {
        Some(port) if port != default_port => Some(format!("{scheme}://{host}:{port}")),
        _ => Some(format!("{scheme}://{host}")),
    }
}

fn split_host_port(authority: &str) -> Option<(&str, Option<&str>)> {
    if let Some(stripped) = authority.strip_prefix('[') {
        // An IPv6 literal: `[::1]:8080`.
        let (host, tail) = stripped.split_once(']')?;
        let port = tail.strip_prefix(':');
        return Some((&authority[..host.len() + 2], port.filter(|p| !p.is_empty())));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) && !port.is_empty() => {
            Some((host, Some(port)))
        }
        Some(_) => None,
        None => Some((authority, None)),
    }
}

/// Whether `origin` is one of `allowed`, comparing in canonical form.
///
/// An entry may be written as a bare origin or as a URL; both reduce to the same origin. An entry that
/// has no origin matches nothing, so a typo in an allow-list fails closed rather than open.
#[must_use]
pub fn origin_allowed(origin: &str, allowed: &[String]) -> bool {
    let Some(origin) = origin_of(origin) else {
        return false;
    };
    allowed
        .iter()
        .filter_map(|entry| origin_of(entry))
        .any(|entry| entry == origin)
}

/// What a navigation is allowed to be (DOMAIN.md §8.4): `http`, `https` or `about:blank`, with no
/// credentials embedded in the URL.
///
/// # Errors
/// Returns [`FaultCode::NavigationDenied`] for any other scheme (`javascript:`, `data:`, `file:`,
/// `blob:`, `chrome:`, an application scheme), for embedded credentials, and for a URL that is not
/// well formed.
pub fn validate_navigation(url: &str) -> Result<(), BrowserFault> {
    let denied = |why: &str| {
        Err(BrowserFault::new(
            FaultCode::NavigationDenied,
            format!("navigation refused: {why}"),
        ))
    };
    if url.trim() != url || url.chars().any(char::is_control) {
        return denied("the URL has surrounding whitespace or control characters");
    }
    if url == "about:blank" {
        return Ok(());
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return denied("only http, https and about:blank destinations are allowed");
    };
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
        return denied("only http, https and about:blank destinations are allowed");
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return denied("the URL embeds credentials");
    }
    if origin_of(url).is_none() {
        return denied("the URL has no valid host");
    }
    Ok(())
}

/// Whether a URL names an external application handoff: a scheme the browser would hand to another
/// program (`mailto:`, `tel:`, an application scheme) rather than render.
#[must_use]
pub fn is_external_handoff(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    let scheme = scheme.to_ascii_lowercase();
    !matches!(
        scheme.as_str(),
        "http"
            | "https"
            | "about"
            | "data"
            | "blob"
            | "javascript"
            | "file"
            | "chrome"
            | "ws"
            | "wss"
            | "view-source"
            | "filesystem"
    ) && scheme
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_are_canonical() {
        assert_eq!(
            origin_of("HTTPS://Example.COM:443/a?b#c").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            origin_of("http://127.0.0.1:8080/x").as_deref(),
            Some("http://127.0.0.1:8080")
        );
        assert_eq!(
            origin_of("http://[::1]:9/").as_deref(),
            Some("http://[::1]:9")
        );
        assert_eq!(origin_of("https://user:pw@example.com/"), None);
        assert_eq!(origin_of("about:blank"), None);
        assert_eq!(origin_of("data:text/html,x"), None);
    }

    #[test]
    fn an_allow_list_entry_with_no_origin_matches_nothing() {
        let allowed = vec!["not a url".to_string(), "https://a.example".to_string()];
        assert!(origin_allowed("https://a.example/path", &allowed));
        assert!(!origin_allowed("https://b.example", &allowed));
        assert!(!origin_allowed("not a url", &allowed));
    }

    #[test]
    fn navigation_is_web_only_without_credentials() {
        for good in [
            "about:blank",
            "http://a.example/",
            "https://a.example:8443/x",
        ] {
            assert!(validate_navigation(good).is_ok(), "{good}");
        }
        for bad in [
            "javascript:alert(1)",
            "data:text/html,hi",
            "file:///etc/passwd",
            "chrome://settings",
            "mailto:a@b.c",
            "https://user:pw@a.example/",
            "https://user@a.example/",
            " https://a.example/",
            "a.example",
            "https:///nohost",
        ] {
            assert!(validate_navigation(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn external_handoffs_are_application_schemes() {
        assert!(is_external_handoff("mailto:a@b.c"));
        assert!(is_external_handoff("slack://open"));
        assert!(!is_external_handoff("https://a.example"));
        assert!(!is_external_handoff("about:blank"));
    }
}
