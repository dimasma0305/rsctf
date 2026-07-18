//! `Ad/Vpn/Config` endpoint — download a WireGuard `.conf` for the caller's A&D
//! participation. Ported from RSCTF's `AdGameController.DownloadVpnConfig`.
//!
//! rsctf persists a unique per-participation peer and programs the in-process
//! WireGuard hub from that same row, so downloaded keys and addresses always
//! match the live cryptokey-routing state.

use axum::http::header;
use axum::response::{IntoResponse, Response};

use super::*;
use crate::services::ad_vpn;

pub(super) struct RosterAccessGuard {
    distributed: crate::utils::single_flight::PgAdvisoryLock,
    local: crate::utils::single_flight::CoalesceGuard,
}

impl RosterAccessGuard {
    pub(super) async fn release(self) -> AppResult<()> {
        let Self { distributed, local } = self;
        distributed.release().await?;
        drop(local);
        Ok(())
    }
}

/// Serialize new credential issuance with roster/team/user revocation, then
/// re-check the live membership and account role without the participation cache.
pub(super) async fn acquire_roster_access(
    st: &SharedState,
    user: &CurrentUser,
    part: &participation::Model,
) -> AppResult<RosterAccessGuard> {
    let key = format!("team-roster:{}", part.team_id);
    let local = crate::utils::single_flight::coalesce(&key).await;
    let distributed = crate::utils::single_flight::PgAdvisoryLock::acquire(st.pg(), &key).await?;
    let authorized = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(
               SELECT 1
                 FROM "UserParticipations" link
                 JOIN "Participations" participation
                   ON participation.id = link.participation_id
                 JOIN "AspNetUsers" account ON account.id = link.user_id
                WHERE link.user_id = $1
                  AND link.game_id = $2
                  AND link.team_id = $3
                  AND link.participation_id = $4
                  AND participation.game_id = $2
                  AND participation.team_id = $3
                  AND participation.status = $5
                  AND account.role <> $6
           )"#,
    )
    .bind(user.id)
    .bind(part.game_id)
    .bind(part.team_id)
    .bind(part.id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(Role::Banned as i16)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !authorized {
        distributed.release().await?;
        drop(local);
        return Err(AppError::Forbidden);
    }
    Ok(RosterAccessGuard { distributed, local })
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

/// Render the WireGuard `.conf` for a `(game, participation, user)` triple from
/// the team's **persisted** `AdVpnPeer` key + the in-process hub's server key, so
/// the downloaded config's keys match the live `wg0` interface and the handshake
/// succeeds. `AllowedIPs` routes the A&D services subnet + the client subnet
/// (other teams' BYOC) over the tunnel. Shared with the BYOC bundle (`byoc.rs`).
pub(super) async fn render_wg_config(
    st: &SharedState,
    game: &game::Model,
    user_name: &str,
    participation_id: i32,
) -> AppResult<String> {
    let peer = ad_vpn::ensure_peer(&st.db, game.id, participation_id).await?;
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
        gid = game.id,
        priv_key = peer.private_key,
        address = peer.address,
    ))
}

/// A filesystem-safe token from a user's display name, for `.conf` filenames.
/// Falls back to `player` when nothing survives the filter.
pub(super) fn safe_user_slug(name: &str) -> String {
    let s: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if s.is_empty() {
        "player".to_string()
    } else {
        s
    }
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
    let roster_access = acquire_roster_access(&st, &user, &part).await?;

    // Game must have at least one A&D or KotH challenge (mirrors RSCTF's hasAd).
    let has_ad = game_challenge::Entity::find()
        .filter(game_challenge::Column::GameId.eq(id))
        .filter(game_challenge::Column::IsEnabled.eq(true))
        .filter(
            game_challenge::Column::ReviewStatus
                .eq(crate::utils::enums::ChallengeReviewStatus::Active),
        )
        .filter(
            game_challenge::Column::ChallengeType
                .eq(ChallengeType::AttackDefense)
                .or(game_challenge::Column::ChallengeType.eq(ChallengeType::KingOfTheHill)),
        )
        .one(&st.db)
        .await?
        .is_some();
    if !has_ad {
        return Err(AppError::not_found(
            "This game has no A&D or KotH challenges",
        ));
    }

    let game = game::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Game not found"))?;

    let conf = render_wg_config(&st, &game, &user.name, part.id).await?;
    let safe_user_name = safe_user_slug(&user.name);
    roster_access.release().await?;

    Ok((
        [
            (header::CONTENT_TYPE, "text/plain".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"ad-game-{id}-{safe_user_name}.conf\""),
            ),
        ],
        conf.into_bytes(),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::merge_allowed_routes;

    #[test]
    fn custom_vpn_routes_cannot_drop_required_service_routes() {
        let routes = merge_allowed_routes(
            Some("192.0.2.0/24, 10.96.0.0/12"),
            vec!["10.96.0.0/12".to_string(), "10.13.0.0/16".to_string()],
        );
        assert_eq!(routes, "192.0.2.0/24, 10.96.0.0/12, 10.13.0.0/16");
    }
}
