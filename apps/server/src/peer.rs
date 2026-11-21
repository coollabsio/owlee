//! Client address for rate limits and analytics.
//!
//! Forwarding headers are used only when the TCP peer is inside
//! `OWLEE_TRUSTED_PROXIES` (loopback and private networks by default). Then
//! `OWLEE_PROXY_HOPS` says how many proxies append to `X-Forwarded-For`, and
//! the client is that many entries from the right. A peer from anywhere else
//! is the client, so a visitor that reaches the port directly cannot fake
//! its address or the Cloudflare geo headers.

use std::convert::Infallible;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{Extensions, HeaderMap};
use owlee_platform::Config;

use crate::state::AppState;

pub struct Client {
    pub ip: String,
    /// Rate-limit key. An IPv6 client is its /64, because one host usually
    /// owns the whole /64 and can rotate through it.
    pub limit_key: String,
    /// `OWLEE_PROXY_HOPS` when the peer is a trusted proxy, else 0.
    pub hops: usize,
}

impl Client {
    pub fn resolve(config: &Config, headers: &HeaderMap, peer: Option<IpAddr>) -> Self {
        let trusted = peer.is_some_and(|ip| {
            config
                .trusted_proxies
                .iter()
                .any(|network| network.contains(&ip))
        });
        if trusted && config.proxy_hops == 0 && headers.contains_key("x-forwarded-for") {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                tracing::warn!(
                    "a trusted proxy sent X-Forwarded-For, but OWLEE_PROXY_HOPS is 0, so every \
                     client behind the proxy shares one rate limit; set OWLEE_PROXY_HOPS to the \
                     number of proxies in front of Owlee"
                );
            });
        }
        let hops = if trusted { config.proxy_hops } else { 0 };
        let addr = forwarded(headers, hops).or(peer);
        let ip = addr.map_or_else(|| "0.0.0.0".to_owned(), |ip| ip.to_string());
        let limit_key = addr.map_or_else(|| ip.clone(), limit_key);
        Self {
            ip,
            limit_key,
            hops,
        }
    }

    pub fn of(config: &Config, headers: &HeaderMap, extensions: &Extensions) -> Self {
        Self::resolve(config, headers, peer_addr(extensions))
    }
}

impl FromRequestParts<AppState> for Client {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self::of(&state.config, &parts.headers, &parts.extensions))
    }
}

/// The `hops`-th address from the right across every `X-Forwarded-For` line.
/// A value that is not an IP address is ignored.
fn forwarded(headers: &HeaderMap, hops: usize) -> Option<IpAddr> {
    if hops == 0 {
        return None;
    }
    let entries: Vec<&str> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .collect();
    entries.iter().rev().nth(hops - 1)?.parse().ok()
}

fn limit_key(addr: IpAddr) -> String {
    match addr.to_canonical() {
        IpAddr::V6(v6) => {
            let network = u128::from(v6) & !((1u128 << 64) - 1);
            format!("{}/64", Ipv6Addr::from(network))
        }
        v4 => v4.to_string(),
    }
}

fn peer_addr(extensions: &Extensions) -> Option<IpAddr> {
    extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(hops: usize) -> Config {
        let mut config = Config::development();
        config.proxy_hops = hops;
        config
    }

    fn headers(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append("x-forwarded-for", value.parse().unwrap());
        }
        headers
    }

    fn ip(config: &Config, xff: &[&str], peer: &str) -> String {
        Client::resolve(config, &headers(xff), Some(peer.parse().unwrap())).ip
    }

    #[test]
    fn one_proxy_uses_the_appended_client() {
        assert_eq!(ip(&config(1), &["203.0.113.9"], "10.0.0.8"), "203.0.113.9");
        assert_eq!(
            ip(&config(1), &["198.51.100.4, 203.0.113.9"], "10.0.0.8"),
            "203.0.113.9"
        );
    }

    #[test]
    fn an_ipv6_client_is_limited_by_its_64() {
        let key = |peer: &str| {
            Client::resolve(&config(0), &HeaderMap::new(), Some(peer.parse().unwrap())).limit_key
        };
        assert_eq!(key("2001:db8:1:2:aaaa::1"), "2001:db8:1:2::/64");
        assert_eq!(key("2001:db8:1:2:bbbb::9"), "2001:db8:1:2::/64");
        assert_eq!(key("::ffff:203.0.113.9"), "203.0.113.9");
        assert_eq!(key("203.0.113.9"), "203.0.113.9");
    }

    #[test]
    fn zero_hops_ignores_the_header() {
        assert_eq!(ip(&config(0), &["203.0.113.9"], "10.0.0.8"), "10.0.0.8");
    }

    #[test]
    fn an_untrusted_peer_cannot_fake_its_address() {
        let client = Client::resolve(
            &config(2),
            &headers(&["198.51.100.1, 203.0.113.9"]),
            Some("198.51.100.77".parse().unwrap()),
        );
        assert_eq!(client.ip, "198.51.100.77");
        assert_eq!(client.hops, 0, "Cloudflare geo headers are not trusted");
    }

    #[test]
    fn every_header_line_counts() {
        // A second line added by the proxy must not be skipped.
        assert_eq!(
            ip(&config(1), &["198.51.100.1", "203.0.113.9"], "10.0.0.8"),
            "203.0.113.9"
        );
    }

    #[test]
    fn a_value_that_is_not_an_address_falls_back_to_the_peer() {
        assert_eq!(ip(&config(1), &["not-an-ip"], "10.0.0.8"), "10.0.0.8");
    }

    #[test]
    fn two_hops_take_the_second_entry_from_the_right() {
        assert_eq!(
            ip(&config(2), &["203.0.113.9, 172.64.0.1"], "10.0.0.8"),
            "203.0.113.9"
        );
    }
}
