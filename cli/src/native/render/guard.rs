//! Request policy for renderers that load documents they did not author.
//!
//! `renderer serve` renders HTML sent by its callers, so it must not trust
//! that HTML: page scripts stay off and every subresource is checked before
//! Chrome fetches it. Without the network check a render request could make
//! the service fetch cloud metadata endpoints or cluster-internal services
//! (SSRF). The check resolves the host and refuses non-public addresses; a
//! network policy around the service remains the hard boundary, because DNS
//! can answer differently when Chrome resolves the host again.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use url::{Host, Url};

/// What a renderer enforces on the documents it loads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RenderPolicy {
    /// Page scripts never run. The daemon already strips them when it
    /// serializes a page, but a service cannot rely on its callers for that.
    pub disable_javascript: bool,
    /// Subresources load only over http(s) from public addresses.
    pub public_network_only: bool,
}

impl RenderPolicy {
    /// In-process renderer of the daemon's own pages: trusted, and allowed to
    /// reach local development servers.
    pub const LOCAL: Self = Self {
        disable_javascript: false,
        public_network_only: false,
    };

    /// `renderer serve`: renders documents from the network.
    pub const SERVICE: Self = Self {
        disable_javascript: true,
        public_network_only: true,
    };
}

/// Why a subresource was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocked {
    Scheme(String),
    Unparseable,
    PrivateAddress(IpAddr),
    Unresolvable,
}

impl std::fmt::Display for Blocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Blocked::Scheme(s) => write!(f, "scheme {} is not allowed", s),
            Blocked::Unparseable => write!(f, "URL could not be parsed"),
            Blocked::PrivateAddress(ip) => write!(f, "address {} is not public", ip),
            Blocked::Unresolvable => write!(f, "host did not resolve"),
        }
    }
}

/// Checks a subresource URL under [`RenderPolicy::public_network_only`].
pub async fn check_public_url(raw: &str) -> Result<(), Blocked> {
    let url = Url::parse(raw).map_err(|_| Blocked::Unparseable)?;
    match url.scheme() {
        // Inline payloads never touch the network.
        "data" | "blob" => return Ok(()),
        "http" | "https" => {}
        other => return Err(Blocked::Scheme(other.to_string())),
    }
    let port = url.port_or_known_default().unwrap_or(80);
    match url.host() {
        Some(Host::Ipv4(ip)) => require_public(IpAddr::V4(ip)),
        Some(Host::Ipv6(ip)) => require_public(IpAddr::V6(ip)),
        Some(Host::Domain(domain)) => {
            let addrs: Vec<IpAddr> = tokio::net::lookup_host((domain, port))
                .await
                .map_err(|_| Blocked::Unresolvable)?
                .map(|a| a.ip())
                .collect();
            if addrs.is_empty() {
                return Err(Blocked::Unresolvable);
            }
            // Every answer must be public: Chrome may pick any of them.
            addrs.into_iter().try_for_each(require_public)
        }
        None => Err(Blocked::Unparseable),
    }
}

fn require_public(ip: IpAddr) -> Result<(), Blocked> {
    if is_public(ip) {
        Ok(())
    } else {
        Err(Blocked::PrivateAddress(ip))
    }
}

pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_public_v4(v4),
            None => is_public_v6(v6),
        },
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || a == 0
        // Carrier-grade NAT, 100.64.0.0/10.
        || (a == 100 && (64..128).contains(&b))
        // IETF protocol assignments, 192.0.0.0/24.
        || (a == 192 && b == 0 && ip.octets()[2] == 0)
        // Benchmarking, 198.18.0.0/15.
        || (a == 198 && (b == 18 || b == 19))
        // Reserved, 240.0.0.0/4.
        || a >= 240)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        // Unique local, fc00::/7.
        || (first & 0xfe00) == 0xfc00
        // Link-local, fe80::/10.
        || (first & 0xffc0) == 0xfe80
        // Documentation, 2001:db8::/32.
        || (first == 0x2001 && ip.segments()[1] == 0x0db8)
        // NAT64 well-known prefix, 64:ff9b::/96, can reach IPv4 internals.
        || (first == 0x0064 && ip.segments()[1] == 0xff9b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn private_and_special_v4_ranges_are_not_public() {
        for s in [
            "127.0.0.1",
            "10.0.0.5",
            "172.31.115.127",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "192.0.0.170",
            "198.18.0.1",
        ] {
            assert!(!is_public(ip(s)), "{s}");
        }
        for s in ["8.8.8.8", "1.1.1.1", "172.32.0.1", "100.128.0.1"] {
            assert!(is_public(ip(s)), "{s}");
        }
    }

    #[test]
    fn private_v6_ranges_and_mapped_v4_are_not_public() {
        for s in [
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::a9fe:a9fe",
        ] {
            assert!(!is_public(ip(s)), "{s}");
        }
        assert!(is_public(ip("2606:4700:4700::1111")));
    }

    #[tokio::test]
    async fn literal_hosts_are_checked_without_dns() {
        assert_eq!(
            check_public_url("http://169.254.169.254/latest/meta-data/").await,
            Err(Blocked::PrivateAddress(ip("169.254.169.254")))
        );
        // WHATWG parsing normalizes integer and hex IPv4 forms.
        assert!(matches!(
            check_public_url("http://2130706433/").await,
            Err(Blocked::PrivateAddress(_))
        ));
        assert!(matches!(
            check_public_url("http://0x7f.1/").await,
            Err(Blocked::PrivateAddress(_))
        ));
        assert!(matches!(
            check_public_url("http://[::ffff:127.0.0.1]:8080/").await,
            Err(Blocked::PrivateAddress(_))
        ));
        assert_eq!(check_public_url("https://1.1.1.1/x.css").await, Ok(()));
    }

    #[tokio::test]
    async fn only_http_and_inline_schemes_pass() {
        assert_eq!(
            check_public_url("file:///etc/passwd").await,
            Err(Blocked::Scheme("file".to_string()))
        );
        assert_eq!(
            check_public_url("ftp://1.1.1.1/").await,
            Err(Blocked::Scheme("ftp".to_string()))
        );
        assert_eq!(check_public_url("data:image/png;base64,AA==").await, Ok(()));
        assert_eq!(
            check_public_url("not a url").await,
            Err(Blocked::Unparseable)
        );
    }

    #[tokio::test]
    async fn localhost_names_resolve_to_blocked_addresses() {
        assert!(matches!(
            check_public_url("http://localhost:9300/").await,
            Err(Blocked::PrivateAddress(_))
        ));
    }
}
