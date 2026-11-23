//! Inbound filters drop noise before it is stored or counted against a cap.

use owlee_errors::NormalizedEvent;
use serde::Serialize;
use serde_json::Value;

use crate::browser::{host_of, is_bot, is_local_host};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InboundFilter {
    BrowserExtensions,
    WebCrawlers,
    Localhost,
    LegacyBrowsers,
}

#[derive(Serialize)]
pub struct FilterInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
}

const EXTENSION_SCHEMES: [&str; 5] = [
    "chrome-extension://",
    "moz-extension://",
    "safari-extension://",
    "safari-web-extension://",
    "ms-browser-extension://",
];

/// Sentry's list of errors thrown by common browser extensions.
const EXTENSION_ERRORS: [&str; 11] = [
    "top.GLOBALS",
    "originalCreateNotification",
    "canvas.contentDocument",
    "MyApp_RemoveAllHighlights",
    "atomicFindClose",
    "conduitPage",
    "Can't find variable: ZiteReader",
    "jigsaw is not defined",
    "ComboSearch is not defined",
    "loading.retry.widdit.com",
    "NS_ERROR_NOT_CONNECTED",
];

impl InboundFilter {
    pub const ALL: [Self; 4] = [
        Self::BrowserExtensions,
        Self::WebCrawlers,
        Self::Localhost,
        Self::LegacyBrowsers,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::BrowserExtensions => "browser_extensions",
            Self::WebCrawlers => "web_crawlers",
            Self::Localhost => "localhost",
            Self::LegacyBrowsers => "legacy_browsers",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::BrowserExtensions => "Browser extension errors",
            Self::WebCrawlers => "Web crawlers",
            Self::Localhost => "Localhost",
            Self::LegacyBrowsers => "Legacy browsers",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::BrowserExtensions => "Errors thrown by browser extensions, not by your code.",
            Self::WebCrawlers => "Errors from search engine bots and other crawlers.",
            Self::Localhost => "Errors from localhost, 127.0.0.1, and ::1.",
            Self::LegacyBrowsers => {
                "Internet Explorer, Opera Presto, Edge Legacy, Chrome below 80, Firefox below 78, and Safari below 14."
            }
        }
    }

    pub fn info(self) -> FilterInfo {
        FilterInfo {
            id: self.id(),
            label: self.label(),
            description: self.description(),
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|filter| filter.id() == id)
    }

    pub fn matches(self, event: &NormalizedEvent) -> bool {
        match self {
            Self::BrowserExtensions => from_extension(event),
            Self::WebCrawlers => user_agent(event).is_some_and(is_bot),
            Self::Localhost => from_localhost(event),
            Self::LegacyBrowsers => user_agent(event).is_some_and(legacy_browser),
        }
    }
}

/// Unknown ids are skipped, so a removed filter needs no migration.
pub fn parse_list(text: &str) -> Vec<InboundFilter> {
    InboundFilter::ALL
        .into_iter()
        .filter(|filter| text.split(',').any(|id| id.trim() == filter.id()))
        .collect()
}

pub fn to_list(filters: &[InboundFilter]) -> String {
    filters
        .iter()
        .map(|filter| filter.id())
        .collect::<Vec<_>>()
        .join(",")
}

fn from_extension(event: &NormalizedEvent) -> bool {
    let crashing_path = event
        .exceptions
        .last()
        .and_then(|exception| exception.frames.last())
        .and_then(|frame| frame.abs_path.as_deref().or(frame.filename.as_deref()));
    if crashing_path.is_some_and(|path| {
        EXTENSION_SCHEMES
            .iter()
            .any(|scheme| path.starts_with(scheme))
    }) {
        return true;
    }
    event
        .exceptions
        .iter()
        .filter_map(|exception| exception.value.as_deref())
        .chain(event.message.as_deref())
        .any(|text| {
            EXTENSION_ERRORS
                .iter()
                .any(|pattern| text.contains(pattern))
        })
}

fn from_localhost(event: &NormalizedEvent) -> bool {
    let url_host = event
        .request
        .get("url")
        .and_then(Value::as_str)
        .and_then(host_of);
    if url_host.as_deref().is_some_and(is_local_host) {
        return true;
    }
    matches!(
        event.user.get("ip_address").and_then(Value::as_str),
        Some("127.0.0.1" | "::1")
    )
}

/// The end user's agent, from the event's `request.headers`. Sentry SDKs send
/// headers as an object or as a list of name and value pairs.
fn user_agent(event: &NormalizedEvent) -> Option<&str> {
    let is_ua = |name: &str| name.eq_ignore_ascii_case("user-agent");
    match event.request.get("headers")? {
        Value::Object(headers) => headers
            .iter()
            .find(|(name, _)| is_ua(name))
            .and_then(|(_, value)| value.as_str()),
        Value::Array(pairs) => pairs.iter().find_map(|pair| {
            let name = pair.get(0)?.as_str()?;
            is_ua(name).then(|| pair.get(1)?.as_str()).flatten()
        }),
        _ => None,
    }
}

fn legacy_browser(ua: &str) -> bool {
    if ["MSIE", "Trident/", "Presto/", "Edge/"]
        .iter()
        .any(|marker| ua.contains(marker))
    {
        return true;
    }
    let below = |marker: &str, min: u32| major_after(ua, marker).is_some_and(|major| major < min);
    if ua.contains("Chrome/") || ua.contains("Chromium/") {
        return below("Chrome/", 80) || below("Chromium/", 80);
    }
    if ua.contains("Firefox/") {
        return below("Firefox/", 78);
    }
    ua.contains("Safari/") && below("Version/", 14)
}

fn major_after(ua: &str, marker: &str) -> Option<u32> {
    let rest = &ua[ua.find(marker)? + marker.len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use owlee_errors::normalize_event;
    use serde_json::json;

    use super::*;

    fn with_ua(ua: &str) -> NormalizedEvent {
        normalize_event(json!({
            "message": "boom",
            "request": {"url": "https://shop.example.com/", "headers": {"User-Agent": ua}}
        }))
    }

    #[test]
    fn list_round_trips_and_skips_unknown_ids() {
        let filters = parse_list("web_crawlers, gone,browser_extensions");
        assert_eq!(
            filters,
            vec![InboundFilter::BrowserExtensions, InboundFilter::WebCrawlers]
        );
        assert_eq!(to_list(&filters), "browser_extensions,web_crawlers");
        assert_eq!(parse_list(""), vec![]);
        assert_eq!(
            InboundFilter::from_id("localhost"),
            Some(InboundFilter::Localhost)
        );
        assert_eq!(InboundFilter::from_id("nope"), None);
    }

    #[test]
    fn browser_extensions() {
        let frame = normalize_event(json!({
            "exception": {"values": [{"type": "TypeError", "value": "x is undefined",
                "stacktrace": {"frames": [
                    {"filename": "https://shop.example.com/app.js"},
                    {"abs_path": "chrome-extension://abcdef/content.js"}
                ]}}]}
        }));
        assert!(InboundFilter::BrowserExtensions.matches(&frame));
        let app_frame = normalize_event(json!({
            "exception": {"values": [{"type": "TypeError", "value": "x is undefined",
                "stacktrace": {"frames": [
                    {"abs_path": "chrome-extension://abcdef/content.js"},
                    {"filename": "https://shop.example.com/app.js"}
                ]}}]}
        }));
        assert!(!InboundFilter::BrowserExtensions.matches(&app_frame));
        let known = normalize_event(json!({
            "exception": {"values": [{"type": "ReferenceError", "value": "jigsaw is not defined"}]}
        }));
        assert!(InboundFilter::BrowserExtensions.matches(&known));
        let message = normalize_event(json!({"message": "Can't find variable: ZiteReader"}));
        assert!(InboundFilter::BrowserExtensions.matches(&message));
        assert!(!InboundFilter::BrowserExtensions.matches(&with_ua("Mozilla/5.0")));
    }

    #[test]
    fn web_crawlers() {
        assert!(InboundFilter::WebCrawlers.matches(&with_ua(
            "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)"
        )));
        assert!(!InboundFilter::WebCrawlers.matches(&with_ua(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) AppleWebKit/537.36 Chrome/128.0.0.0 Safari/537.36"
        )));
        let pairs = normalize_event(json!({
            "request": {"headers": [["user-agent", "Googlebot/2.1"]]}
        }));
        assert!(InboundFilter::WebCrawlers.matches(&pairs));
        let none = normalize_event(json!({"message": "from curl"}));
        assert!(!InboundFilter::WebCrawlers.matches(&none));
    }

    #[test]
    fn localhost() {
        let local_url = normalize_event(json!({"request": {"url": "http://localhost:3000/cart"}}));
        assert!(InboundFilter::Localhost.matches(&local_url));
        let local_ip = normalize_event(json!({"user": {"ip_address": "::1"}}));
        assert!(InboundFilter::Localhost.matches(&local_ip));
        let public = normalize_event(json!({
            "request": {"url": "https://shop.example.com/"},
            "user": {"ip_address": "203.0.113.9"}
        }));
        assert!(!InboundFilter::Localhost.matches(&public));
    }

    #[test]
    fn legacy_browsers() {
        for old in [
            "Mozilla/5.0 (Windows NT 6.1; Trident/7.0; rv:11.0) like Gecko",
            "Mozilla/4.0 (compatible; MSIE 8.0; Windows NT 6.1)",
            "Opera/9.80 (Windows NT 6.1) Presto/2.12.388 Version/12.16",
            "Mozilla/5.0 (Windows NT 10.0) AppleWebKit/537.36 Chrome/70.0.3538.102 Safari/537.36 Edge/18.19582",
            "Mozilla/5.0 (Windows NT 10.0) AppleWebKit/537.36 Chrome/79.0.3945.88 Safari/537.36",
            "Mozilla/5.0 (Windows NT 10.0; rv:77.0) Gecko/20100101 Firefox/77.0",
            "Mozilla/5.0 (Macintosh) AppleWebKit/605.1.15 Version/13.1.2 Safari/605.1.15",
        ] {
            assert!(
                InboundFilter::LegacyBrowsers.matches(&with_ua(old)),
                "{old}"
            );
        }
        for current in [
            "Mozilla/5.0 (Windows NT 10.0) AppleWebKit/537.36 Chrome/128.0.0.0 Safari/537.36 Edg/128.0.0.0",
            "Mozilla/5.0 (Windows NT 10.0; rv:128.0) Gecko/20100101 Firefox/128.0",
            "Mozilla/5.0 (Macintosh) AppleWebKit/605.1.15 Version/17.4 Safari/605.1.15",
            "Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36 Version/4.0 Chrome/128.0.0.0 Mobile Safari/537.36",
        ] {
            assert!(
                !InboundFilter::LegacyBrowsers.matches(&with_ua(current)),
                "{current}"
            );
        }
    }
}
