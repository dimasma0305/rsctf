//! Bounded operational signals for persistent A&D publication/checker incidents.

use crate::app_state::SharedState;

const INCIDENT_THRESHOLD_ROUNDS: usize = 3;
const INCIDENT_REMINDER_ROUNDS: i32 = 20;
const HISTORY_ROWS: i64 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RoundHealth {
    number: i32,
    delivery_failures: i32,
    flag_count: i64,
    check_count: i64,
    healthy_checks: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IncidentPhase {
    None,
    Started,
    Sustained,
    Reminder,
    Suppressed,
}

fn incident_phase(
    history: &[RoundHealth],
    affected: impl Fn(&RoundHealth) -> bool,
) -> IncidentPhase {
    let Some(current) = history.first().filter(|row| affected(row)) else {
        return IncidentPhase::None;
    };
    let consecutive = history.iter().take_while(|row| affected(row)).count();
    if consecutive == 1 {
        return IncidentPhase::Started;
    }
    if consecutive >= INCIDENT_THRESHOLD_ROUNDS {
        let crossed_threshold = history
            .get(INCIDENT_THRESHOLD_ROUNDS)
            .is_none_or(|row| !affected(row));
        if crossed_threshold {
            return IncidentPhase::Sustained;
        }
        if current.number.rem_euclid(INCIDENT_REMINDER_ROUNDS) == 0 {
            return IncidentPhase::Reminder;
        }
    }
    IncidentPhase::Suppressed
}

fn report_delivery(game_id: i32, row: RoundHealth, phase: IncidentPhase) {
    let complete_outage = row.flag_count > 0 && i64::from(row.delivery_failures) >= row.flag_count;
    match phase {
        IncidentPhase::None | IncidentPhase::Suppressed => {}
        IncidentPhase::Started => tracing::warn!(
            game = game_id,
            round = row.number,
            failed = row.delivery_failures,
            services = row.flag_count,
            complete_outage,
            "cron: A&D flag-delivery incident started"
        ),
        IncidentPhase::Sustained => tracing::error!(
            game = game_id,
            round = row.number,
            failed = row.delivery_failures,
            services = row.flag_count,
            complete_outage,
            consecutive_rounds = INCIDENT_THRESHOLD_ROUNDS,
            "cron: sustained A&D flag-delivery failure; operator action required"
        ),
        IncidentPhase::Reminder => tracing::error!(
            game = game_id,
            round = row.number,
            failed = row.delivery_failures,
            services = row.flag_count,
            complete_outage,
            "cron: sustained A&D flag-delivery incident remains unresolved"
        ),
    }
}

fn report_checker(game_id: i32, row: RoundHealth, phase: IncidentPhase) {
    match phase {
        IncidentPhase::None | IncidentPhase::Suppressed => {}
        IncidentPhase::Started => tracing::warn!(
            game = game_id,
            round = row.number,
            healthy = row.healthy_checks,
            checks = row.check_count,
            "cron: all A&D/KotH checker observations are unhealthy"
        ),
        IncidentPhase::Sustained => tracing::error!(
            game = game_id,
            round = row.number,
            healthy = row.healthy_checks,
            checks = row.check_count,
            consecutive_rounds = INCIDENT_THRESHOLD_ROUNDS,
            "cron: sustained all-service checker failure; operator action required"
        ),
        IncidentPhase::Reminder => tracing::error!(
            game = game_id,
            round = row.number,
            healthy = row.healthy_checks,
            checks = row.check_count,
            "cron: sustained all-service checker incident remains unresolved"
        ),
    }
}

pub(super) async fn report(
    state: &SharedState,
    game_id: i32,
    round_number: i32,
    current_delivery_failures: i32,
) {
    let rows = sqlx::query_as::<_, (i32, i32, i64, i64, i64)>(
        r#"SELECT round.number, round.flag_delivery_failures,
                  (SELECT COUNT(*) FROM "AdFlags" flag
                    WHERE flag.round_id = round.id),
                  (SELECT COUNT(*) FROM "AdCheckResults" result
                    WHERE result.round_id = round.id),
                  (SELECT COUNT(*) FROM "AdCheckResults" result
                    WHERE result.round_id = round.id AND result.status = $2)
             FROM "AdRounds" round
            WHERE round.game_id = $1
              AND round.flags_published_at IS NOT NULL
            ORDER BY round.number DESC
            LIMIT $3"#,
    )
    .bind(game_id)
    .bind(crate::services::ad_engine::AdCheckStatus::Ok as i16)
    .bind(HISTORY_ROWS)
    .fetch_all(state.pg())
    .await;
    let rows = match rows {
        Ok(rows) => rows
            .into_iter()
            .map(
                |(number, delivery_failures, flag_count, check_count, healthy_checks)| {
                    RoundHealth {
                        number,
                        delivery_failures,
                        flag_count,
                        check_count,
                        healthy_checks,
                    }
                },
            )
            .collect::<Vec<_>>(),
        Err(error) => {
            if current_delivery_failures > 0 {
                tracing::warn!(
                    game = game_id,
                    round = round_number,
                    failed = current_delivery_failures,
                    %error,
                    "cron: A&D flag delivery failed and incident history could not be read"
                );
            }
            return;
        }
    };
    let Some(current) = rows
        .first()
        .copied()
        .filter(|row| row.number == round_number)
    else {
        if current_delivery_failures > 0 {
            tracing::warn!(
                game = game_id,
                round = round_number,
                failed = current_delivery_failures,
                "cron: A&D flag delivery failed before incident history became visible"
            );
        }
        return;
    };

    let delivery_phase = incident_phase(&rows, |row| row.delivery_failures > 0);
    report_delivery(game_id, current, delivery_phase);
    let checker_phase = incident_phase(&rows, |row| row.check_count > 0 && row.healthy_checks == 0);
    report_checker(game_id, current, checker_phase);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health(number: i32, delivery_failures: i32, healthy_checks: i64) -> RoundHealth {
        RoundHealth {
            number,
            delivery_failures,
            flag_count: 10,
            check_count: 10,
            healthy_checks,
        }
    }

    #[test]
    fn incident_emits_start_threshold_and_bounded_reminders() {
        assert_eq!(
            incident_phase(&[health(1, 1, 10)], |row| row.delivery_failures > 0),
            IncidentPhase::Started
        );
        assert_eq!(
            incident_phase(
                &[health(3, 1, 10), health(2, 1, 10), health(1, 1, 10)],
                |row| row.delivery_failures > 0
            ),
            IncidentPhase::Sustained
        );
        assert_eq!(
            incident_phase(
                &[
                    health(19, 1, 10),
                    health(18, 1, 10),
                    health(17, 1, 10),
                    health(16, 1, 10)
                ],
                |row| row.delivery_failures > 0
            ),
            IncidentPhase::Suppressed
        );
        assert_eq!(
            incident_phase(
                &[
                    health(20, 1, 10),
                    health(19, 1, 10),
                    health(18, 1, 10),
                    health(17, 1, 10)
                ],
                |row| row.delivery_failures > 0
            ),
            IncidentPhase::Reminder
        );
    }

    #[test]
    fn healthy_round_resets_each_incident_signal() {
        let history = [
            health(8, 0, 10),
            health(7, 1, 0),
            health(6, 1, 0),
            health(5, 1, 0),
        ];
        assert_eq!(
            incident_phase(&history, |row| row.delivery_failures > 0),
            IncidentPhase::None
        );
        assert_eq!(
            incident_phase(&history, |row| {
                row.check_count > 0 && row.healthy_checks == 0
            }),
            IncidentPhase::None
        );
    }
}
