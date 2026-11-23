//! Origin, Referer, and User-Agent checks shared by analytics and error ingest.

use std::sync::OnceLock;

use isbot::Bots;
use owlee_insights::clean_host;

/// Refuses a request with neither `Origin` nor `Referer`, as analytics needs a page.
pub fn origin_allowed(
    origin: Option<&str>,
    referer: Option<&str>,
    allowed: &[String],
    allow_localhost: bool,
) -> bool {
    request_host(origin, referer).is_some_and(|host| host_allowed(&host, allowed, allow_localhost))
}

pub fn request_host(origin: Option<&str>, referer: Option<&str>) -> Option<String> {
    origin
        .and_then(host_of)
        .or_else(|| referer.and_then(host_of))
}

/// An entry allows its own host and every subdomain of it.
pub fn host_allowed(host: &str, allowed: &[String], allow_localhost: bool) -> bool {
    if allow_localhost && is_local_host(host) {
        return true;
    }
    allowed
        .iter()
        .any(|domain| host == domain || host.ends_with(&format!(".{domain}")))
}

pub fn host_of(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let rest = value
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(value);
    let hostport = rest.split(['/', '?', '#']).next()?;
    if let Some(host) = hostport.strip_prefix('[') {
        return Some(host.split(']').next()?.to_ascii_lowercase());
    }
    clean_host(hostport).ok()
}

pub fn is_local_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

pub fn is_bot(user_agent: &str) -> bool {
    static BOTS: OnceLock<Bots> = OnceLock::new();
    BOTS.get_or_init(Bots::default).is_bot(user_agent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_match_subdomains() {
        let allowed = vec!["example.com".to_owned()];
        assert!(host_allowed("example.com", &allowed, false));
        assert!(host_allowed("app.example.com", &allowed, false));
        assert!(!host_allowed("badexample.com", &allowed, false));
        assert!(!host_allowed("localhost", &allowed, false));
        assert!(host_allowed("localhost", &allowed, true));
    }

    #[test]
    fn host_comes_from_origin_then_referer() {
        assert_eq!(
            request_host(Some("https://Shop.Example.com:8443"), None).as_deref(),
            Some("shop.example.com")
        );
        assert_eq!(
            request_host(None, Some("http://[::1]:3000/page?x=1")).as_deref(),
            Some("::1")
        );
        assert_eq!(request_host(None, None), None);
        assert!(!origin_allowed(
            None,
            None,
            &["example.com".to_owned()],
            true
        ));
    }
}
