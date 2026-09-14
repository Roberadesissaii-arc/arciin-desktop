//! Turning what a person types into a URL we are willing to talk to,
//! and deciding later whether a navigation is still inside that server.

use url::{Host, Url};

/// Why an address was refused before any network request was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddressError {
    /// Nothing but whitespace.
    Empty,
    /// A scheme we will never fetch (`javascript:`, `data:`, `file:`, ...).
    UnsupportedScheme(String),
    /// Syntactically unusable as a host.
    Invalid,
}

impl AddressError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Empty => "ADDRESS_EMPTY",
            Self::UnsupportedScheme(_) => "ADDRESS_SCHEME_UNSUPPORTED",
            Self::Invalid => "ADDRESS_INVALID",
        }
    }
}

/// Only these two schemes are ever fetched or navigated to.
fn is_web_scheme(scheme: &str) -> bool {
    matches!(scheme, "http" | "https")
}

/// Hosts we treat as "this is a LAN box", and therefore default to plain HTTP
/// for — which is a supported Arciin deployment.
fn is_local_host(host: &Host<&str>) -> bool {
    match host {
        Host::Ipv4(ip) => {
            ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified()
        }
        Host::Ipv6(ip) => ip.is_loopback() || (ip.segments()[0] & 0xffc0) == 0xfe80,
        Host::Domain(name) => {
            let lowered = name.to_ascii_lowercase();
            lowered == "localhost"
                || lowered.ends_with(".local")
                || lowered.ends_with(".localhost")
                || lowered.ends_with(".home.arpa")
        }
    }
}

/// Reduce a URL to scheme + host + port. Paths, queries, fragments and
/// credentials are dropped: a server is an origin, not a page.
fn to_origin(mut url: Url) -> Result<Url, AddressError> {
    if !is_web_scheme(url.scheme()) {
        return Err(AddressError::UnsupportedScheme(url.scheme().to_string()));
    }
    if url.host().is_none() {
        return Err(AddressError::Invalid);
    }
    url.set_path("");
    url.set_query(None);
    url.set_fragment(None);
    let _ = url.set_username("");
    let _ = url.set_password(None);
    Ok(url)
}

/// The ordered list of origins to try for a typed address.
///
/// Rules:
/// - An explicit scheme is honoured exactly. `https://` is **never**
///   downgraded to `http://`, whatever happens on the wire.
/// - A bare LAN address (`192.168.1.50`, `arciin.local`) becomes `http://` —
///   plain HTTP on a trusted local network is a supported Arciin setup.
/// - A bare public hostname is tried over `https://` first, then `http://`.
///   Preferring TLS for a routable name costs nothing and never weakens a
///   connection the user asked to be secure.
pub fn candidate_origins(input: &str) -> Result<Vec<Url>, AddressError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(AddressError::Empty);
    }

    // Reject a dangerous scheme before `Url` has a chance to accept it.
    if let Some((scheme, _)) = trimmed.split_once("://") {
        if !is_web_scheme(&scheme.to_ascii_lowercase()) {
            return Err(AddressError::UnsupportedScheme(scheme.to_ascii_lowercase()));
        }
    } else if let Some((scheme, _)) = trimmed.split_once(':') {
        // `javascript:alert(1)` has no `//`. A bare `host:port` does not
        // match, because a port is all digits.
        let scheme = scheme.to_ascii_lowercase();
        let rest = &trimmed[scheme.len() + 1..];
        let looks_like_port = !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit());
        if !looks_like_port && !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphabetic())
        {
            return Err(AddressError::UnsupportedScheme(scheme));
        }
    }

    if trimmed.contains("://") {
        let url = Url::parse(trimmed).map_err(|_| AddressError::Invalid)?;
        return Ok(vec![to_origin(url)?]);
    }

    // No scheme: decide the default from the shape of the host.
    let probe = Url::parse(&format!("http://{trimmed}")).map_err(|_| AddressError::Invalid)?;
    let host = probe.host().ok_or(AddressError::Invalid)?;
    let local = is_local_host(&host);
    let http = to_origin(probe)?;

    if local {
        return Ok(vec![http]);
    }

    let https = Url::parse(&format!("https://{trimmed}")).map_err(|_| AddressError::Invalid)?;
    Ok(vec![to_origin(https)?, http])
}

/// The canonical string form of an origin: `http://192.168.1.50`,
/// `https://arciin.example.com:8443`. Used for storage, logging and
/// same-origin comparison, so it must be stable.
pub fn origin_string(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// What the user sees on a server card. The scheme is noise for a LAN box on
/// its default port; a port or TLS is worth showing.
pub fn friendly_address(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match (url.scheme(), url.port()) {
        ("http", None) => host.to_string(),
        ("http", Some(port)) => format!("{host}:{port}"),
        (scheme, None) => format!("{scheme}://{host}"),
        (scheme, Some(port)) => format!("{scheme}://{host}:{port}"),
    }
}

/// Whether a URL the WebView wants to load still belongs to the connected
/// server. Scheme, host and port must all match; anything else is external.
pub fn is_same_origin(allowed: &Url, candidate: &str) -> bool {
    let Ok(parsed) = Url::parse(candidate) else {
        return false;
    };
    if !is_web_scheme(parsed.scheme()) {
        return false;
    }
    parsed.origin() == allowed.origin()
}

/// Whether a URL is safe to hand to the operating system's browser.
/// `javascript:` and `data:` must never reach a shell open.
pub fn is_safe_external(candidate: &str) -> bool {
    Url::parse(candidate)
        .map(|url| is_web_scheme(url.scheme()))
        .unwrap_or(false)
}
