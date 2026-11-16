//! Advertised SSH address: the private web ingress does not serve SSH.

pub(crate) fn listen_port() -> u16 {
    std::env::var("RSCTF_AD_SSH_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2222)
}

fn select_host(
    explicit: Option<String>,
    hub: Option<String>,
    public: Option<String>,
) -> Option<String> {
    explicit
        .filter(|host| !host.is_empty())
        .or(hub)
        .or_else(|| public.filter(|host| !host.is_empty()))
}

/// Same-origin VPN clients reach SSH at the hub, while the website hostname
/// resolves to the separate HTTPS ingress. An explicit SSH override wins.
pub fn jump_host() -> Option<String> {
    let hub = crate::services::ad_vpn::same_origin_access()
        .ok()
        .flatten()
        .map(|access| access.dns.to_string());
    let host = select_host(
        std::env::var("RSCTF_AD_SSH_PUBLIC_HOST").ok(),
        hub,
        std::env::var("RSCTF_DOCKER_PUBLIC_ENTRY").ok(),
    )?;
    Some(format!("{host}:{}", listen_port()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_ssh_does_not_use_the_split_dns_website() {
        let hub = Some("10.13.0.1".to_owned());
        let website = Some("ctf.example.test".to_owned());
        assert_eq!(select_host(None, hub.clone(), website.clone()), hub);
        assert_eq!(select_host(Some(String::new()), hub.clone(), website), hub);
    }

    #[test]
    fn explicit_ssh_host_and_non_vpn_deployments_keep_their_addresses() {
        let explicit = Some("ssh.example.test".to_owned());
        let website = Some("ctf.example.test".to_owned());
        assert_eq!(
            select_host(explicit.clone(), Some("10.13.0.1".into()), website.clone()),
            explicit,
        );
        assert_eq!(select_host(None, None, website.clone()), website);
        assert_eq!(select_host(None, None, Some(String::new())), None);
    }
}
