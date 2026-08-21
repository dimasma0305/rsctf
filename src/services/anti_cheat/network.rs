//! Trusted reverse-proxy handling for account identity capture.

use std::net::IpAddr;

use axum::http::HeaderMap;

use super::{ip_hint, network_prefix, normalize_ip, normalize_ip_addr, parse_ip};

/// Apply the same privacy boundary to observation and legacy block displays.
/// Unrecognized inputs never pass through verbatim.
pub fn redacted_identity_hint(kind: &str, value: &str) -> String {
    if kind.eq_ignore_ascii_case("ip") {
        if let Some(ip) = parse_ip(value) {
            return ip_hint(ip);
        }
        if let Some(prefix) = value.strip_suffix(".x") {
            let candidate = format!("{prefix}.0");
            if let Some(IpAddr::V4(ip)) = parse_ip(&candidate) {
                return ip_hint(IpAddr::V4(ip));
            }
        }
        if let Some((address, "64")) = value.split_once('/') {
            if let Some(IpAddr::V6(ip)) = parse_ip(address) {
                return network_prefix(IpAddr::V6(ip), 64);
            }
        }
    } else if kind.eq_ignore_ascii_case("fingerprint") {
        let Some(prefix) = value.strip_suffix('…') else {
            return "masked".to_string();
        };
        if prefix.len() == 12 && prefix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return format!("{}…", prefix.to_ascii_lowercase());
        }
    }
    "masked".to_string()
}

/// Client IP from a forwarded chain only when the immediate socket peer is an
/// explicitly trusted proxy. `X-Real-IP` is deliberately ignored: unlike XFF,
/// several proxies pass that non-standard incoming header through unchanged.
pub fn client_ip(headers: &HeaderMap, peer: Option<IpAddr>) -> Option<String> {
    client_ip_with_trusted_networks(headers, peer, trusted_proxy_networks())
}

fn client_ip_with_trusted_networks(
    headers: &HeaderMap,
    peer: Option<IpAddr>,
    trusted_networks: &[ipnet::IpNet],
) -> Option<String> {
    let peer = peer.map(normalize_ip_addr)?;
    if !trusted_networks
        .iter()
        .any(|network| network.contains(&peer))
    {
        return Some(normalize_ip(peer));
    }
    let xff_values = headers
        .get_all("x-forwarded-for")
        .iter()
        .collect::<Vec<_>>();
    if xff_values.is_empty() {
        return Some(normalize_ip(peer));
    }
    // Multiple XFF field-lines are semantically one comma-separated list in
    // wire order. Walk field-lines and their values right-to-left so a proxy
    // that appends a second field cannot be hidden by an attacker-controlled
    // malformed first field.
    for field in xff_values.iter().rev() {
        let Ok(field) = field.to_str() else {
            return Some(normalize_ip(peer));
        };
        for value in field.rsplit(',').map(str::trim) {
            let Some(hop) = parse_ip(value).map(normalize_ip_addr) else {
                return Some(normalize_ip(peer));
            };
            if !trusted_networks
                .iter()
                .any(|network| network.contains(&hop))
            {
                return Some(normalize_ip(hop));
            }
        }
    }
    Some(normalize_ip(peer))
}

fn parse_trusted_proxy_cidrs(value: &str) -> anyhow::Result<Vec<ipnet::IpNet>> {
    value
        .split(|character: char| character == ',' || character.is_ascii_whitespace())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<ipnet::IpNet>()
                .map_err(|_| anyhow::anyhow!("invalid trusted proxy CIDR: {value}"))
        })
        .collect()
}

fn trusted_proxy_networks() -> &'static [ipnet::IpNet] {
    static NETWORKS: std::sync::LazyLock<Vec<ipnet::IpNet>> = std::sync::LazyLock::new(|| {
        let configured = std::env::var("RSCTF_TRUSTED_PROXY_CIDRS").unwrap_or_default();
        parse_trusted_proxy_cidrs(&configured).unwrap_or_else(|error| {
            tracing::error!(%error, "ignoring invalid trusted-proxy configuration");
            Vec::new()
        })
    });
    &NETWORKS
}

pub fn validate_trusted_proxy_config() -> anyhow::Result<()> {
    let configured = std::env::var("RSCTF_TRUSTED_PROXY_CIDRS").unwrap_or_default();
    parse_trusted_proxy_cidrs(&configured).map(|_| ())
}

pub fn configured_trusted_proxy_cidrs() -> Vec<String> {
    trusted_proxy_networks()
        .iter()
        .map(ToString::to_string)
        .collect()
}

pub fn is_trusted_proxy(peer: IpAddr) -> bool {
    let peer = normalize_ip_addr(peer);
    trusted_proxy_networks()
        .iter()
        .any(|network| network.contains(&peer))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use std::net::Ipv4Addr;

    #[test]
    fn untrusted_peer_cannot_spoof_forwarded_ip() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.9"));
        let peer = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7));
        assert_eq!(
            client_ip_with_trusted_networks(&headers, Some(peer), &[]).as_deref(),
            Some("198.51.100.7")
        );
    }

    #[test]
    fn single_trusted_proxy_uses_forwarded_client() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.9"));
        let peer = "10.0.0.2".parse().unwrap();
        let trusted = vec!["10.0.0.0/8".parse().unwrap()];
        assert_eq!(
            client_ip_with_trusted_networks(&headers, Some(peer), &trusted).as_deref(),
            Some("203.0.113.9")
        );
    }

    #[test]
    fn forwarded_chain_skips_multiple_trusted_hops_and_spoofed_prefix() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("192.0.2.66, 203.0.113.9, 10.1.0.3, 10.2.0.4"),
        );
        let trusted = vec!["10.0.0.0/8".parse().unwrap()];
        assert_eq!(
            client_ip_with_trusted_networks(&headers, Some("10.3.0.5".parse().unwrap()), &trusted)
                .as_deref(),
            Some("203.0.113.9")
        );
    }

    #[test]
    fn x_real_ip_is_never_an_identity_source() {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", HeaderValue::from_static("203.0.113.9"));
        let peer = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));
        let trusted = vec!["10.0.0.0/8".parse().unwrap()];
        assert_eq!(
            client_ip_with_trusted_networks(&headers, Some(peer), &trusted).as_deref(),
            Some("10.0.0.2")
        );
    }

    #[test]
    fn malformed_forwarded_values_fall_back_to_peer() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("not-an-ip"));
        let peer = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));
        let trusted = vec!["10.0.0.0/8".parse().unwrap()];
        assert_eq!(
            client_ip_with_trusted_networks(&headers, Some(peer), &trusted).as_deref(),
            Some("10.0.0.2")
        );
    }

    #[test]
    fn malformed_spoofed_prefix_does_not_hide_a_valid_client_hop() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("garbage, 203.0.113.9, 10.1.0.3"),
        );
        let trusted = vec!["10.0.0.0/8".parse().unwrap()];
        assert_eq!(
            client_ip_with_trusted_networks(&headers, Some("10.2.0.4".parse().unwrap()), &trusted)
                .as_deref(),
            Some("203.0.113.9")
        );
    }

    #[test]
    fn proxy_appended_second_field_ignores_attacker_malformed_first_field() {
        let mut headers = HeaderMap::new();
        headers.append("x-forwarded-for", HeaderValue::from_static("garbage"));
        headers.append(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.9, 10.1.0.3"),
        );
        let trusted = vec!["10.0.0.0/8".parse().unwrap()];
        assert_eq!(
            client_ip_with_trusted_networks(&headers, Some("10.2.0.4".parse().unwrap()), &trusted)
                .as_deref(),
            Some("203.0.113.9")
        );
    }

    #[test]
    fn ipv4_mapped_proxy_and_client_are_normalized_before_trust_checks() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("::ffff:203.0.113.9"),
        );
        let trusted = vec!["10.0.0.0/8".parse().unwrap()];
        assert_eq!(
            client_ip_with_trusted_networks(
                &headers,
                Some("::ffff:10.0.0.2".parse().unwrap()),
                &trusted
            )
            .as_deref(),
            Some("203.0.113.9")
        );
    }

    #[test]
    fn trusted_proxy_parser_is_explicit_and_strict() {
        assert!(parse_trusted_proxy_cidrs("").unwrap().is_empty());
        let networks = parse_trusted_proxy_cidrs("192.0.2.10/32, 2001:db8::1/128").unwrap();
        assert_eq!(networks.len(), 2);
        assert!(networks[0].contains(&"192.0.2.10".parse::<IpAddr>().unwrap()));
        assert!(!networks[0].contains(&"192.0.2.11".parse::<IpAddr>().unwrap()));
        assert!(parse_trusted_proxy_cidrs("192.0.2.10").is_err());
    }
}
