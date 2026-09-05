//! `Ad/Vpn/Config` endpoint — download a WireGuard `.conf` for the caller's A&D
//! participation. Ported from RSCTF's `AdGameController.DownloadVpnConfig`.
//!
//! Player downloads always use a personal event peer, independently of the API
//! VPN gate. Only BYOC hosting bundles use the shared participation peer.

use axum::response::Response;

use super::*;
use crate::services::ad_vpn;

pub(crate) struct RosterAccessGuard {
    distributed: crate::utils::single_flight::PgAdvisoryLock,
    local: crate::utils::single_flight::CoalesceGuard,
    _admission: tokio::sync::OwnedSemaphorePermit,
}

impl RosterAccessGuard {
    pub(crate) fn transaction_mut(&mut self) -> &mut sqlx::Transaction<'static, sqlx::Postgres> {
        self.distributed.transaction_mut()
    }

    pub(crate) async fn release(self) -> AppResult<()> {
        let Self {
            distributed,
            local,
            _admission,
        } = self;
        distributed.release().await?;
        drop(local);
        drop(_admission);
        Ok(())
    }
}

/// Serialize new credential issuance with roster/team/user revocation, then
/// re-check the live membership and account role without the participation cache.
pub(crate) async fn acquire_roster_access(
    st: &SharedState,
    user: &CurrentUser,
    part: &participation::Model,
) -> AppResult<RosterAccessGuard> {
    // Serialize one team's issuers before consuming global capacity: a flood
    // from one team must not occupy both slots with one request merely waiting
    // on the same local gate. Neither wait retains a database connection.
    let key = format!("team-roster:{}", part.team_id);
    let local = crate::utils::single_flight::coalesce(&key).await;

    // This guard remains live while VPN/BYOC/SSH/token issuance performs
    // follow-up queries. Global admission still precedes the database checkout,
    // bounding retained roster transactions across distinct teams.
    let admission = crate::utils::single_flight::roster_access_permit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let mut distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire(st.pg(), &key).await?;
    let authorized = crate::services::ad::roster::user_allows_shared_credentials_on(
        &mut *distributed.transaction_mut(),
        user.id,
        &user.security_stamp,
        part.game_id,
        part.team_id,
        part.id,
    )
    .await?;
    if !authorized {
        distributed.release().await?;
        drop(local);
        return Err(AppError::Forbidden);
    }
    Ok(RosterAccessGuard {
        distributed,
        local,
        _admission: admission,
    })
}

fn merge_allowed_routes(configured: Option<&str>, required: Vec<String>) -> String {
    let mut routes: Vec<String> = configured
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|route| !route.is_empty())
        .map(str::to_string)
        .collect();
    for route in required {
        if !routes.contains(&route) {
            routes.push(route);
        }
    }
    routes.join(", ")
}

fn wireguard_comment(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

/// Render the WireGuard `.conf` for a `(game, participation, user)` triple from
/// the team's **persisted** `AdVpnPeer` key + the in-process hub's server key, so
/// the downloaded config's keys match the live `wg0` interface and the handshake
/// succeeds. `AllowedIPs` routes the A&D services subnet + the client subnet
/// (other teams' BYOC) over the tunnel. Shared with the BYOC bundle (`byoc.rs`).
pub(super) async fn render_wg_config_for_game(
    st: &SharedState,
    game_id: i32,
    user_name: &str,
    participation_id: i32,
) -> AppResult<String> {
    let peer = ad_vpn::ensure_peer(&st.db, game_id, participation_id).await?;
    if peer.address.is_empty() {
        return Err(AppError::internal(
            "Could not assign a VPN address for this team",
        ));
    }
    let server_pub = ad_vpn::server_public_key(&st.db).await?;
    let listen_port = ad_vpn::listen_port();
    let dns = std::env::var("RSCTF_AD_VPN_DNS").unwrap_or_else(|_| "1.1.1.1".to_string());

    // Public UDP endpoint teams dial: explicit override, else the public entry host.
    let endpoint = std::env::var("RSCTF_AD_VPN_SERVER_ENDPOINT")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let host = std::env::var("RSCTF_DOCKER_PUBLIC_ENTRY")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "127.0.0.1".to_string());
            format!("{host}:{listen_port}")
        });

    // Route the platform services subnet + the client subnet (BYOC teammates).
    let configured_routes = std::env::var("RSCTF_AD_VPN_ALLOWED_IPS").ok();
    let mut required_routes = ad_vpn::service_route_cidrs().map_err(AppError::internal)?;
    required_routes.push(ad_vpn::client_cidr());
    let allowed_ips = merge_allowed_routes(configured_routes.as_deref(), required_routes);

    let generated = Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
    let user_name = wireguard_comment(user_name);
    Ok(format!(
        "# WireGuard config for {name} — A&D game {gid}\n\
         # Generated {generated}\n\
         \n\
         [Interface]\n\
         PrivateKey = {priv_key}\n\
         Address = {address}/32\n\
         DNS = {dns}\n\
         \n\
         [Peer]\n\
         PublicKey = {server_pub}\n\
         Endpoint = {endpoint}\n\
         AllowedIPs = {allowed_ips}\n\
         PersistentKeepalive = 25\n",
        name = user_name,
        gid = game_id,
        priv_key = peer.private_key,
        address = peer.address,
    ))
}

/// `GET /api/Game/{id}/Ad/Vpn/Config` — download the caller's WireGuard `.conf`.
///
/// Gated on the caller being an **accepted** participant of the game (via
/// `resolve_participation`) and the game actually having an A&D or KotH
/// challenge. Returns the config as `text/plain` with an attachment filename,
/// matching RSCTF's field layout (Interface PrivateKey/Address/DNS; Peer
/// PublicKey/Endpoint/AllowedIPs/PersistentKeepalive).
pub async fn download_vpn_config(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<Response> {
    let part = resolve_participation(&st, &user, id).await?;

    // Game must have at least one A&D or KotH challenge (mirrors RSCTF's hasAd).
    let has_ad: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(SELECT 1 FROM "GameChallenges"
             WHERE game_id = $1 AND is_enabled = TRUE AND review_status = 0
               AND "Type" IN (4, 5))"#,
    )
    .bind(id)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !has_ad {
        return Err(AppError::not_found(
            "This game has no A&D or KotH challenges",
        ));
    }

    // Sharing one WireGuard key makes teammates roam the same kernel endpoint,
    // interrupting each other's TCP/UDP traffic. API access policy must not
    // choose a shared transport identity. BYOC keeps its separate hosting peer.
    let conf = crate::services::event_security::render_user_config(&st, &user, &part).await?;
    Ok(crate::controllers::game::event_vpn_config_response(
        id, conf,
    ))
}

#[cfg(test)]
mod tests {
    use super::{merge_allowed_routes, wireguard_comment};

    #[test]
    fn custom_vpn_routes_cannot_drop_required_service_routes() {
        let routes = merge_allowed_routes(
            Some("192.0.2.0/24, 10.96.0.0/12"),
            vec!["10.96.0.0/12".to_string(), "10.13.0.0/16".to_string()],
        );
        assert_eq!(routes, "192.0.2.0/24, 10.96.0.0/12, 10.13.0.0/16");
    }

    #[test]
    fn display_name_cannot_add_wg_quick_directives() {
        let name = wireguard_comment("player\r\nPostUp = touch /tmp/pwned\0");
        assert_eq!(name, "player  PostUp = touch /tmp/pwned ");
        assert!(name
            .chars()
            .all(|character| !matches!(character, '\r' | '\n' | '\0')));
    }
}
