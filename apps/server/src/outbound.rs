//! Keeps notification webhooks away from addresses inside the server's network.
//!
//! A member who saves a webhook URL chooses where this server sends a POST.
//! Without this check that URL could be a cloud metadata endpoint or an
//! internal service. IP literals are refused before the request, and host
//! names resolve through [`PublicOnly`], so the address that is checked is
//! the address that is dialed. Monitors are not covered: checking internal
//! targets is their job.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

pub const PRIVATE_REFUSED: &str = "This URL points to a private or local address. Set OWLEE_ALLOW_PRIVATE_WEBHOOKS=true to allow it.";

/// `Err` when the URL host is an IP literal that is not public.
pub fn check_url(url: &str) -> Result<(), &'static str> {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return Ok(());
    };
    let host = parsed.host_str().unwrap_or("");
    let Ok(ip) = host.trim_start_matches('[').trim_end_matches(']').parse() else {
        return Ok(());
    };
    if is_public(ip) {
        Ok(())
    } else {
        Err(PRIVATE_REFUSED)
    }
}

pub fn is_public(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(ip) => is_public_v4(ip),
        IpAddr::V6(ip) => is_public_v6(ip),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        || a >= 240
        || (a == 100 && (b & 0xc0) == 64)
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (b & 0xfe) == 18))
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (s[0] & 0xfe00) == 0xfc00
        || (s[0] & 0xffc0) == 0xfe80
        || (s[0] & 0xffc0) == 0xfec0
        || (s[0] == 0x2001 && s[1] == 0x0db8)
        || (s[0] == 0x2001 && s[1] == 0)
        || (s[0] == 0x0064 && s[1] == 0xff9b && s[2] == 1)
        || (s[0] == 0x0100 && s[1] == 0 && s[2] == 0 && s[3] == 0))
}

/// System DNS, with every non-public answer removed.
pub struct PublicOnly;

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let public: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|addr| is_public(addr.ip()))
                .collect();
            if public.is_empty() {
                return Err(PRIVATE_REFUSED.into());
            }
            Ok(Box::new(public.into_iter()) as Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{check_url, is_public};

    #[test]
    fn classifies_addresses() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "198.18.0.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn refuses_private_literals_only() {
        assert!(check_url("http://169.254.169.254/latest/meta-data").is_err());
        assert!(check_url("http://[::1]:8080/hook").is_err());
        assert!(check_url("https://1.1.1.1/hook").is_ok());
        assert!(check_url("https://hooks.slack.com/services/x").is_ok());
    }
}
