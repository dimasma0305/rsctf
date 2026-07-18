use sqlx::PgConnection;

use crate::services::ad_engine::AdCheckStatus;
use crate::utils::error::{AppError, AppResult};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ObservedToken {
    pub id: i32,
    pub participation_id: i32,
    pub window_round: i32,
}

#[derive(Clone, Debug)]
pub(crate) struct ClaimObservation<'a> {
    pub game_id: i32,
    pub challenge_id: i32,
    pub target_id: i32,
    pub cycle_id: i64,
    pub container_id: &'a str,
    pub ad_round_id: i32,
    pub token: Option<ObservedToken>,
    pub status: AdCheckStatus,
    pub confirmation_ticks: i32,
    pub token_window_complete: bool,
    pub claimant_is_eligible: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ClaimProjection {
    token_id: Option<i32>,
    provisional_participation_id: Option<i32>,
    streak: i32,
    confirmed_participation_id: Option<i32>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ClaimOutcome {
    pub controller: Option<i32>,
    pub responsible: Option<i32>,
    pub provisional: Option<i32>,
    pub confirmed: Option<i32>,
    pub confirmation_streak: i32,
    pub token_id: Option<i32>,
    pub token_window_round: Option<i32>,
    pub acquisition_confirmed: bool,
    pub is_scorable: bool,
}

fn transition_claim(
    previous: ClaimProjection,
    token: Option<ObservedToken>,
    status: AdCheckStatus,
    confirmation_ticks: i32,
    token_window_complete: bool,
    claimant_is_eligible: bool,
) -> (ClaimProjection, ClaimOutcome) {
    if status == AdCheckStatus::InternalError || !token_window_complete {
        return (
            previous,
            ClaimOutcome {
                confirmed: previous.confirmed_participation_id,
                responsible: previous.confirmed_participation_id,
                is_scorable: false,
                ..ClaimOutcome::default()
            },
        );
    }

    let token = token.filter(|_| claimant_is_eligible);
    let Some(token) = token else {
        let next = ClaimProjection {
            token_id: None,
            provisional_participation_id: None,
            streak: 0,
            ..previous
        };
        return (
            next,
            ClaimOutcome {
                responsible: previous.confirmed_participation_id,
                confirmed: previous.confirmed_participation_id,
                is_scorable: true,
                ..ClaimOutcome::default()
            },
        );
    };

    let same_claim = previous.token_id == Some(token.id)
        && previous.provisional_participation_id == Some(token.participation_id);
    let healthy = status == AdCheckStatus::Ok;
    let streak = if healthy {
        if same_claim {
            previous.streak.saturating_add(1)
        } else {
            1
        }
    } else {
        0
    };
    let threshold = confirmation_ticks.max(1);
    let was_confirmed = previous.confirmed_participation_id == Some(token.participation_id)
        && previous.token_id == Some(token.id);
    let newly_confirmed = healthy && streak >= threshold && !was_confirmed;
    let confirmed = if healthy && streak >= threshold {
        Some(token.participation_id)
    } else {
        previous.confirmed_participation_id
    };
    let provisional = (confirmed != Some(token.participation_id)).then_some(token.participation_id);
    let next = ClaimProjection {
        token_id: Some(token.id),
        provisional_participation_id: provisional,
        streak,
        confirmed_participation_id: confirmed,
    };
    (
        next,
        ClaimOutcome {
            controller: Some(token.participation_id),
            responsible: Some(token.participation_id),
            provisional,
            confirmed,
            confirmation_streak: streak,
            token_id: Some(token.id),
            token_window_round: Some(token.window_round),
            acquisition_confirmed: newly_confirmed,
            is_scorable: true,
        },
    )
}

/// Apply one exact-container observation while the caller holds the game/hill
/// lifecycle transaction. The projection is mutable; the acquisition receipt
/// and the caller's `KothControlResults` row are immutable evidence.
pub(crate) async fn apply_observation(
    connection: &mut PgConnection,
    observation: ClaimObservation<'_>,
) -> AppResult<ClaimOutcome> {
    let previous = sqlx::query_as::<_, (Option<i32>, Option<i32>, i32, Option<i32>)>(
        r#"SELECT token_id, provisional_participation_id,
                  confirmation_streak, confirmed_participation_id
             FROM "KothClaimStates"
            WHERE target_id = $1 AND cycle_id = $2 AND container_id = $3
            FOR UPDATE"#,
    )
    .bind(observation.target_id)
    .bind(observation.cycle_id)
    .bind(observation.container_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .map_or_else(ClaimProjection::default, |row| ClaimProjection {
        token_id: row.0,
        provisional_participation_id: row.1,
        streak: row.2.max(0),
        confirmed_participation_id: row.3,
    });
    let (next, outcome) = transition_claim(
        previous,
        observation.token,
        observation.status,
        observation.confirmation_ticks,
        observation.token_window_complete,
        observation.claimant_is_eligible,
    );

    // Infrastructure voids pause the projection exactly where it was.
    if outcome.is_scorable {
        sqlx::query(
            r#"INSERT INTO "KothClaimStates"
                 (target_id, cycle_id, container_id,
                  token_id, token_window_round, provisional_participation_id,
                  confirmation_streak, confirmed_participation_id, updated_at)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,clock_timestamp())
               ON CONFLICT (target_id) DO UPDATE SET
                 cycle_id = EXCLUDED.cycle_id,
                 container_id = EXCLUDED.container_id,
                 token_id = EXCLUDED.token_id,
                 token_window_round = EXCLUDED.token_window_round,
                 provisional_participation_id = EXCLUDED.provisional_participation_id,
                 confirmation_streak = EXCLUDED.confirmation_streak,
                 confirmed_participation_id = EXCLUDED.confirmed_participation_id,
                 updated_at = clock_timestamp()"#,
        )
        .bind(observation.target_id)
        .bind(observation.cycle_id)
        .bind(observation.container_id)
        .bind(next.token_id)
        .bind(observation.token.map(|token| token.window_round))
        .bind(next.provisional_participation_id)
        .bind(next.streak)
        .bind(next.confirmed_participation_id)
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }

    if outcome.acquisition_confirmed {
        let token = observation
            .token
            .expect("a confirmed acquisition always has an exact token");
        sqlx::query(
            r#"INSERT INTO "KothAcquisitions"
                 (game_id, challenge_id, cycle_id, participation_id, token_id,
                  target_id, token_window_round, container_id, ad_round_id,
                  confirmed_at)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,clock_timestamp())
               ON CONFLICT (cycle_id, token_id) DO NOTHING"#,
        )
        .bind(observation.game_id)
        .bind(observation.challenge_id)
        .bind(observation.cycle_id)
        .bind(token.participation_id)
        .bind(token.id)
        .bind(observation.target_id)
        .bind(token.window_round)
        .bind(observation.container_id)
        .bind(observation.ad_round_id)
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(id: i32, participation_id: i32) -> ObservedToken {
        ObservedToken {
            id,
            participation_id,
            window_round: 7,
        }
    }

    #[test]
    fn acquisition_requires_consecutive_healthy_observations() {
        let (one, first) = transition_claim(
            ClaimProjection::default(),
            Some(token(10, 2)),
            AdCheckStatus::Ok,
            2,
            true,
            true,
        );
        assert_eq!(first.provisional, Some(2));
        assert!(!first.acquisition_confirmed);
        let (_, second) =
            transition_claim(one, Some(token(10, 2)), AdCheckStatus::Ok, 2, true, true);
        assert_eq!(second.confirmed, Some(2));
        assert!(second.acquisition_confirmed);
    }

    #[test]
    fn broken_verdict_interrupts_confirmation() {
        let (one, _) = transition_claim(
            ClaimProjection::default(),
            Some(token(10, 2)),
            AdCheckStatus::Ok,
            2,
            true,
            true,
        );
        let (broken, outcome) = transition_claim(
            one,
            Some(token(10, 2)),
            AdCheckStatus::Mumble,
            2,
            true,
            true,
        );
        assert_eq!(outcome.confirmation_streak, 0);
        let (_, retry) =
            transition_claim(broken, Some(token(10, 2)), AdCheckStatus::Ok, 2, true, true);
        assert_eq!(retry.confirmation_streak, 1);
        assert!(!retry.acquisition_confirmed);
    }

    #[test]
    fn a_different_token_restarts_the_streak() {
        let (one, _) = transition_claim(
            ClaimProjection::default(),
            Some(token(10, 2)),
            AdCheckStatus::Ok,
            2,
            true,
            true,
        );
        let (_, stolen) =
            transition_claim(one, Some(token(11, 3)), AdCheckStatus::Ok, 2, true, true);
        assert_eq!(stolen.provisional, Some(3));
        assert_eq!(stolen.confirmation_streak, 1);
    }

    #[test]
    fn internal_error_and_incomplete_issuance_are_void_and_pause() {
        let prior = ClaimProjection {
            token_id: Some(10),
            provisional_participation_id: Some(2),
            streak: 1,
            confirmed_participation_id: None,
        };
        for (status, complete) in [
            (AdCheckStatus::InternalError, true),
            (AdCheckStatus::Ok, false),
        ] {
            let (next, outcome) =
                transition_claim(prior, Some(token(10, 2)), status, 2, complete, true);
            assert_eq!(next, prior);
            assert!(!outcome.is_scorable);
        }
    }

    #[test]
    fn cooldown_marker_is_ineligible_at_application_layer() {
        let (_, outcome) = transition_claim(
            ClaimProjection::default(),
            Some(token(10, 2)),
            AdCheckStatus::Ok,
            2,
            true,
            false,
        );
        assert_eq!(outcome.controller, None);
        assert_eq!(outcome.provisional, None);
    }

    #[test]
    fn confirmed_token_never_awards_twice_in_projection() {
        let (one, _) = transition_claim(
            ClaimProjection::default(),
            Some(token(10, 2)),
            AdCheckStatus::Ok,
            1,
            true,
            true,
        );
        let (_, repeated) =
            transition_claim(one, Some(token(10, 2)), AdCheckStatus::Ok, 1, true, true);
        assert!(!repeated.acquisition_confirmed);
    }
}
