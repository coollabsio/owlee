//! Browser hardening headers on every response.
//!
//! Public status and share pages may be embedded on other sites, so they allow any frame
//! ancestor. The replay player is framed by the dashboard only. The dashboard and
//! everything else refuse framing.

use std::sync::LazyLock;

use axum::extract::Request;
use axum::http::header::{
    CONTENT_SECURITY_POLICY, HeaderMap, HeaderName, HeaderValue, REFERRER_POLICY,
    X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::middleware::Next;
use axum::response::Response;
use base64::Engine as _;
use sha2::{Digest, Sha256};

static APP_POLICY: LazyLock<HeaderValue> = LazyLock::new(|| policy("'none'"));
static EMBEDDABLE_POLICY: LazyLock<HeaderValue> = LazyLock::new(|| policy("*"));
/// Recorded DOM is rebuilt in rrweb's sandboxed inner frame, which inherits
/// this policy: recorded inline styles work, and nothing loads from or posts
/// to another host.
const PLAYER_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
     img-src data: blob:; font-src data:; connect-src 'none'; form-action 'none'; base-uri 'none'; \
     frame-ancestors 'self'";
pub const PLAYER_PATH: &str = "/replay-player.html";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PathClass {
    App,
    Embeddable,
    Player,
}

pub async fn add(request: Request, next: Next) -> Response {
    let class = classify(request.uri().path());
    let mut response = next.run(request).await;
    apply(response.headers_mut(), class);
    response
}

fn apply(headers: &mut HeaderMap, class: PathClass) {
    let (policy, frame_options) = match class {
        PathClass::App => (APP_POLICY.clone(), Some("DENY")),
        PathClass::Embeddable => (EMBEDDABLE_POLICY.clone(), None),
        PathClass::Player => (HeaderValue::from_static(PLAYER_POLICY), Some("SAMEORIGIN")),
    };
    headers.entry(CONTENT_SECURITY_POLICY).or_insert(policy);
    // For browsers without CSP `frame-ancestors`.
    if let Some(frame_options) = frame_options {
        headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static(frame_options));
    }
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers
        .entry(REFERRER_POLICY)
        .or_insert(HeaderValue::from_static("strict-origin-when-cross-origin"));
    headers.insert(
        HeaderName::from_static("cross-origin-opener-policy"),
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=(), payment=()"),
    );
}

fn classify(path: &str) -> PathClass {
    if path == PLAYER_PATH {
        PathClass::Player
    } else if is_embeddable(path) {
        PathClass::Embeddable
    } else {
        PathClass::App
    }
}

/// `/status/{slug}` and `/share/{kind}/{slug}`. The dashboard's own `/status` is not public.
fn is_embeddable(path: &str) -> bool {
    ["/status/", "/share/"].iter().any(|prefix| {
        path.strip_prefix(prefix)
            .is_some_and(|rest| !rest.is_empty())
    })
}

/// The app bundle and its fonts load from this origin. The one inline script in
/// `index.html` (the saved theme, applied before first paint) is allowed by its hash.
/// Theme variables and React `style` props go through the CSSOM, which CSP does not limit.
fn policy(frame_ancestors: &str) -> HeaderValue {
    let scripts = crate::static_assets::inline_scripts()
        .map(|script| {
            let hash = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(script));
            format!(" 'sha256-{hash}'")
        })
        .collect::<String>();
    let policy = format!(
        "default-src 'self'; script-src 'self'{scripts}; style-src 'self'; img-src 'self'; \
         object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors {frame_ancestors}"
    );
    HeaderValue::from_str(&policy).expect("a CSP built from base64 hashes is a valid header")
}

#[cfg(test)]
mod tests {
    use super::{PathClass, classify, is_embeddable};

    #[test]
    fn only_public_pages_are_embeddable() {
        assert!(is_embeddable("/status/acme"));
        assert!(is_embeddable("/share/site/acme"));
        assert!(!is_embeddable("/status"));
        assert!(!is_embeddable("/status/"));
        assert!(!is_embeddable("/"));
        assert!(!is_embeddable("/api/public/acme"));
    }

    #[test]
    fn only_the_player_document_is_the_player_class() {
        assert_eq!(classify("/replay-player.html"), PathClass::Player);
        assert_eq!(classify("/replay-player.html/x"), PathClass::App);
        assert_eq!(classify("/assets/replay-player-abc.js"), PathClass::App);
        assert_eq!(classify("/status/acme"), PathClass::Embeddable);
    }
}
