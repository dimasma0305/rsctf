//! Honeypot cheat signals (`HoneypotHit` + `HoneypotChain`).
//!
//! [`record_honeypot_hit`] is called by [`crate::controllers::honeypot`] whenever
//! a decoy bait route is hit: it persists a `HoneypotHits` row and, when the
//! caller is an authenticated player with an active Accepted participation in a
//! running game, raises `HoneypotHit` against that participation (RSCTF
//! `HoneypotService.RecordHit` — no IP fallback for a browser-forgeable GET, so an
//! unattributable hit is logged but not scored). [`run_honeypot_chain_checks`]
//! ports `HoneypotChainDetectorService`: a participation that trips
//! `CHAIN_THRESHOLD` distinct baits inside `CHAIN_WINDOW` earns `HoneypotChain`.

use std::collections::{HashMap, HashSet};

use chrono::{Duration, Utc};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::models::data::{game, honeypot_hit, participation, user_participation};
use crate::utils::enums::ParticipationStatus;
use crate::utils::error::{AppError, AppResult};

use super::{SuspicionType, GLOBAL_EVIDENCE_KEY};

/// RSCTF `HoneypotConfig.ChainThreshold` default.
const CHAIN_THRESHOLD: usize = 3;
/// RSCTF `HoneypotConfig.ChainWindowMinutes` default.
const CHAIN_WINDOW_MINUTES: i64 = 30;

/// Persist one hit while retaining a share lock on its attributed scoring
/// identity. If cleanup already removed that identity, keep the forensic hit
/// but store it without a dangling participation id.
#[allow(clippy::too_many_arguments)]
async fn persist_honeypot_hit(
    st: &SharedState,
    attributed: Option<(i32, i32)>,
    user_id: Option<uuid::Uuid>,
    bait: &str,
    remote_ip: &str,
    user_agent: Option<&str>,
) -> AppResult<bool> {
    let mut transaction = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let identity_exists = match attributed {
        Some((game_id, participation_id)) => {
            crate::services::participation_evidence::lock_audit_insert_scope(
                &mut transaction,
                game_id,
                None,
                &[participation_id],
            )
            .await?
        }
        None => true,
    };
    let durable_attribution = attributed.filter(|_| identity_exists);
    sqlx::query(
        r#"INSERT INTO "HoneypotHits"
               (game_id, participation_id, user_id, bait, remote_ip,
                user_agent, hit_at_utc)
           VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
    )
    .bind(durable_attribution.map(|(game_id, _)| game_id))
    .bind(durable_attribution.map(|(_, participation_id)| participation_id))
    .bind(user_id)
    .bind(bait)
    .bind(remote_ip)
    .bind(user_agent)
    .bind(Utc::now())
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(identity_exists)
}

/// Record a honeypot bait hit + raise `HoneypotHit` when attributable.
pub async fn record_honeypot_hit(
    st: &SharedState,
    user: Option<CurrentUser>,
    bait: &str,
    remote_ip: Option<String>,
    user_agent: Option<String>,
) {
    let user_id = user.as_ref().map(|u| u.id);

    // Attribute to the caller's active Accepted participation in a live game.
    // (Same-origin authenticated caller; an anonymous / cross-site-forged GET stays
    // unattributed — logged but never scored, matching RSCTF.)
    let mut attributed: Option<(i32, i32)> = None;
    if let Some(uid) = user_id {
        let now = Utc::now();
        let links = user_participation::Entity::find()
            .filter(user_participation::Column::UserId.eq(uid))
            .all(&st.db)
            .await
            .unwrap_or_default();
        for link in links {
            let Ok(Some(p)) = participation::Entity::find_by_id(link.participation_id)
                .one(&st.db)
                .await
            else {
                continue;
            };
            if p.status != ParticipationStatus::Accepted {
                continue;
            }
            if let Ok(Some(g)) = game::Entity::find_by_id(p.game_id).one(&st.db).await {
                if g.start_time_utc <= now && now <= g.end_time_utc {
                    attributed = Some((p.game_id, p.id));
                    break;
                }
            }
        }
    }

    let remote_ip = remote_ip.unwrap_or_default();
    if persist_honeypot_hit(
        st,
        attributed,
        user_id,
        bait,
        &remote_ip,
        user_agent.as_deref(),
    )
    .await
    .is_ok_and(|identity_exists| !identity_exists)
    {
        attributed = None;
    }

    if let Some((game_id, part_id)) = attributed {
        let mut codes = Vec::new();
        let _ = super::detectors::record_with_dedup(
            &st.db,
            game_id,
            part_id,
            None,
            SuspicionType::HoneypotHit,
            GLOBAL_EVIDENCE_KEY,
            &mut codes,
        )
        .await;
    }
}

/// Record a honeypot TCP-listener hit (RSCTF `HoneypotService.RecordTcpHit`).
///
/// Unlike an HTTP bait, a raw TCP connection is NOT browser-forgeable, so the IP
/// fallback is kept: if exactly one player recently logged in from `remote_ip`
/// (per the login `Logs`), the hit is attributed to their live participation and
/// raises `HoneypotProtocolHit`. The `HoneypotHits` row is written regardless.
pub async fn record_honeypot_tcp_hit(st: &SharedState, bait: &str, remote_ip: Option<String>) {
    use crate::models::data::{log_entry, user};

    let ip = remote_ip.clone().unwrap_or_default();

    // Single-user-per-IP attribution: the distinct users who logged in from this IP.
    let mut attributed: Option<(i32, i32, uuid::Uuid)> = None;
    if !ip.is_empty() {
        let names: Vec<String> = log_entry::Entity::find()
            .filter(log_entry::Column::RemoteIp.eq(ip.clone()))
            .filter(log_entry::Column::Logger.contains("AccountController"))
            .all(&st.db)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|l| l.user_name)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        // Only attribute when the IP maps to exactly one account (else ambiguous).
        if names.len() == 1 {
            if let Ok(Some(u)) = user::Entity::find()
                .filter(user::Column::UserName.eq(names[0].clone()))
                .one(&st.db)
                .await
            {
                let now = Utc::now();
                let links = user_participation::Entity::find()
                    .filter(user_participation::Column::UserId.eq(u.id))
                    .all(&st.db)
                    .await
                    .unwrap_or_default();
                for link in links {
                    if let Ok(Some(p)) = participation::Entity::find_by_id(link.participation_id)
                        .one(&st.db)
                        .await
                    {
                        if p.status == ParticipationStatus::Accepted {
                            if let Ok(Some(g)) =
                                game::Entity::find_by_id(p.game_id).one(&st.db).await
                            {
                                if g.start_time_utc <= now && now <= g.end_time_utc {
                                    attributed = Some((p.game_id, p.id, u.id));
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if persist_honeypot_hit(
        st,
        attributed.map(|(game_id, participation_id, _)| (game_id, participation_id)),
        attributed.map(|(_, _, user_id)| user_id),
        bait,
        &ip,
        None,
    )
    .await
    .is_ok_and(|identity_exists| !identity_exists)
    {
        attributed = None;
    }

    if let Some((game_id, part_id, _)) = attributed {
        let mut codes = Vec::new();
        let _ = super::detectors::record_with_dedup(
            &st.db,
            game_id,
            part_id,
            None,
            SuspicionType::HoneypotProtocolHit,
            GLOBAL_EVIDENCE_KEY,
            &mut codes,
        )
        .await;
    }
}

/// Sweep the honeypot hit log for chained baits (RSCTF `HoneypotChainDetectorService`):
/// any participation that tripped `CHAIN_THRESHOLD` DISTINCT baits inside the
/// trailing `CHAIN_WINDOW` earns `HoneypotChain`.
pub async fn run_honeypot_chain_checks(st: &SharedState, game_id: i32) -> AppResult<()> {
    let since = Utc::now() - Duration::minutes(CHAIN_WINDOW_MINUTES);
    let hits = honeypot_hit::Entity::find()
        .filter(honeypot_hit::Column::GameId.eq(game_id))
        .filter(honeypot_hit::Column::HitAtUtc.gte(since))
        .all(&st.db)
        .await?;

    let mut distinct_baits: HashMap<i32, HashSet<String>> = HashMap::new();
    for h in hits {
        if let Some(pid) = h.participation_id {
            distinct_baits.entry(pid).or_default().insert(h.bait);
        }
    }

    for (pid, baits) in distinct_baits {
        if baits.len() >= CHAIN_THRESHOLD {
            let mut codes = Vec::new();
            super::detectors::record_with_dedup(
                &st.db,
                game_id,
                pid,
                None,
                SuspicionType::HoneypotChain,
                GLOBAL_EVIDENCE_KEY,
                &mut codes,
            )
            .await?;
        }
    }
    Ok(())
}
