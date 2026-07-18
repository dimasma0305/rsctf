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
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::models::data::{game, honeypot_hit, participation, user_participation};
use crate::utils::enums::ParticipationStatus;
use crate::utils::error::AppResult;

use super::{SuspicionType, GLOBAL_EVIDENCE_KEY};

/// RSCTF `HoneypotConfig.ChainThreshold` default.
const CHAIN_THRESHOLD: usize = 3;
/// RSCTF `HoneypotConfig.ChainWindowMinutes` default.
const CHAIN_WINDOW_MINUTES: i64 = 30;

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

    let hit = honeypot_hit::ActiveModel {
        game_id: Set(attributed.map(|(g, _)| g)),
        participation_id: Set(attributed.map(|(_, p)| p)),
        user_id: Set(user_id),
        bait: Set(bait.to_string()),
        remote_ip: Set(remote_ip.unwrap_or_default()),
        user_agent: Set(user_agent),
        hit_at_utc: Set(Utc::now()),
        ..Default::default()
    };
    let _ = hit.insert(&st.db).await;

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

    let hit = honeypot_hit::ActiveModel {
        game_id: Set(attributed.map(|(g, _, _)| g)),
        participation_id: Set(attributed.map(|(_, p, _)| p)),
        user_id: Set(attributed.map(|(_, _, u)| u)),
        bait: Set(bait.to_string()),
        remote_ip: Set(ip),
        user_agent: Set(None),
        hit_at_utc: Set(Utc::now()),
        ..Default::default()
    };
    let _ = hit.insert(&st.db).await;

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
