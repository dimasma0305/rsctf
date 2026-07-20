//! Best-effort flag-egress detection for proxied game containers.

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::models::data::{flag_context, game_instance};

use super::{GameAccess, InstanceAccess};

/// Context for the in-tunnel flag-egress scan. Cloneable so the
/// fire-and-forget recorder task can own a copy.
#[derive(Clone)]
pub(super) struct EgressScan {
    pool: sqlx::PgPool,
    /// The owning team's current flag bytes for this challenge.
    pub(super) flag: Vec<u8>,
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
    container_id: Uuid,
    remote_ip: String,
}

/// Stream matcher retaining only the suffix that can begin a flag match in the
/// next read. Its memory use is fixed after construction and never grows with
/// the lifetime or byte volume of a proxy session.
pub(super) struct RollingFlagMatcher {
    overlap: Vec<u8>,
    max_overlap: usize,
}

impl RollingFlagMatcher {
    pub(super) fn new(flag: &[u8]) -> Self {
        let max_overlap = flag.len().saturating_sub(1);
        Self {
            overlap: Vec::with_capacity(max_overlap),
            max_overlap,
        }
    }

    /// Returns whether `chunk` completes a flag wholly within this read or
    /// across its boundary with prior reads.
    pub(super) fn contains(&mut self, flag: &[u8], chunk: &[u8]) -> bool {
        if flag.is_empty() {
            return false;
        }

        let within_chunk = chunk.windows(flag.len()).any(|window| window == flag);
        let max_left = self.overlap.len().min(flag.len().saturating_sub(1));
        let across_boundary = (1..=max_left).any(|left| {
            let right = flag.len() - left;
            right <= chunk.len()
                && self.overlap.ends_with(&flag[..left])
                && chunk.starts_with(&flag[left..])
        });

        self.retain_suffix(chunk);
        within_chunk || across_boundary
    }

    fn retain_suffix(&mut self, chunk: &[u8]) {
        if self.max_overlap == 0 {
            self.overlap.clear();
            return;
        }
        if chunk.len() >= self.max_overlap {
            self.overlap.clear();
            self.overlap
                .extend_from_slice(&chunk[chunk.len() - self.max_overlap..]);
            return;
        }

        let excess = self
            .overlap
            .len()
            .saturating_add(chunk.len())
            .saturating_sub(self.max_overlap);
        if excess > 0 {
            self.overlap.drain(..excess);
        }
        self.overlap.extend_from_slice(chunk);
    }
}

const RECORD_FLAG_EGRESS_SQL: &str = r#"
    INSERT INTO "FlagEgressEvents"
        (game_id, participation_id, challenge_id, container_id,
         remote_ip, remote_port, hit_count, first_seen_utc, last_seen_utc)
    VALUES ($1, $2, $3, $4, $5, $6, 1, $7, $7)
    ON CONFLICT
        (game_id, participation_id, challenge_id,
         (COALESCE(container_id::TEXT, ''::TEXT)), remote_ip, remote_port)
    DO UPDATE SET
        hit_count = LEAST(
            "FlagEgressEvents".hit_count::BIGINT + 1,
            2147483647
        )::INTEGER,
        last_seen_utc = GREATEST(
            "FlagEgressEvents".last_seen_utc,
            EXCLUDED.last_seen_utc
        )
"#;

/// Load the owning team's flag for a proxied instance. `None` disables the
/// scan when there is no per-team flag or the context cannot be resolved.
pub(super) async fn build_egress_scan(
    st: &SharedState,
    access: &InstanceAccess,
    game: &GameAccess,
    remote_ip: String,
) -> Option<EgressScan> {
    let instance = game_instance::Entity::find()
        .filter(game_instance::Column::ParticipationId.eq(game.owner_participation_id))
        .filter(game_instance::Column::ChallengeId.eq(game.challenge_id))
        .one(&st.db)
        .await
        .ok()
        .flatten()?;
    let flag = flag_context::Entity::find_by_id(instance.flag_id?)
        .one(&st.db)
        .await
        .ok()
        .flatten()?
        .flag;
    if flag.is_empty() {
        return None;
    }
    Some(EgressScan {
        pool: st.pg().clone(),
        flag: flag.into_bytes(),
        game_id: game.game_id,
        participation_id: game.owner_participation_id,
        challenge_id: game.challenge_id,
        container_id: access.container_id,
        remote_ip,
    })
}

/// Windowed best-effort upsert of a `FlagEgressEvent`.
pub(super) async fn record_flag_egress(scan: &EgressScan) {
    let Ok(mut transaction) = scan.pool.begin().await else {
        return;
    };
    let Ok(true) = crate::services::participation_evidence::lock_audit_insert_scope(
        &mut transaction,
        scan.game_id,
        Some(scan.challenge_id),
        &[scan.participation_id],
    )
    .await
    else {
        return;
    };
    let now = chrono::Utc::now();
    if sqlx::query(RECORD_FLAG_EGRESS_SQL)
        .bind(scan.game_id)
        .bind(scan.participation_id)
        .bind(scan.challenge_id)
        .bind(scan.container_id)
        .bind(&scan.remote_ip)
        .bind(0_i32)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .is_ok()
    {
        let _ = transaction.commit().await;
    }
}

#[cfg(test)]
mod tests {
    use super::{RollingFlagMatcher, RECORD_FLAG_EGRESS_SQL};

    #[test]
    fn matches_a_flag_at_every_read_boundary() {
        let flag = b"flag{split-across-tcp-reads}";
        for split in 1..flag.len() {
            let mut matcher = RollingFlagMatcher::new(flag);
            assert!(!matcher.contains(flag, &flag[..split]));
            assert!(matcher.contains(flag, &flag[split..]), "split={split}");
        }
    }

    #[test]
    fn matches_across_multiple_reads_and_keeps_only_bounded_overlap() {
        let flag = b"flag{three-reads}";
        let mut matcher = RollingFlagMatcher::new(flag);
        assert!(!matcher.contains(flag, b"noise-flag{"));
        assert!(!matcher.contains(flag, b"three-"));
        assert!(matcher.contains(flag, b"reads}-tail"));
        assert!(matcher.overlap.len() < flag.len());

        for _ in 0..100 {
            assert!(!matcher.contains(flag, b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"));
            assert!(matcher.overlap.len() < flag.len());
        }
    }

    #[test]
    fn atomic_upsert_keys_forensic_usage_by_remote_endpoint() {
        assert!(RECORD_FLAG_EGRESS_SQL.contains("INSERT INTO \"FlagEgressEvents\""));
        assert!(RECORD_FLAG_EGRESS_SQL.contains("(COALESCE(container_id::TEXT, ''::TEXT))"));
        assert!(RECORD_FLAG_EGRESS_SQL.contains("ON CONFLICT"));
        assert!(RECORD_FLAG_EGRESS_SQL.contains("hit_count::BIGINT + 1"));
    }
}
