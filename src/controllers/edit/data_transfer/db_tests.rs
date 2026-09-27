//! Real-PostgreSQL round trip of a competition data archive through the full
//! migrated schema: export a seeded multi-format event, restore it as a new
//! game, and prove every table and every board come back identical.

use std::io::{Cursor, Read, Write};
use std::str::FromStr;
use std::sync::Arc;

use sea_orm::SqlxPostgresConnector;
use serde_json::{json, Value as JsonValue};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::spec::{catalog, Restore};
use super::*;
use crate::app_state::AppState;
use crate::controllers::edit::persist_game_import_with;
use crate::migrations::{test_process_application_name, Migrator, MigratorTrait};
use crate::models::internal::configs::AppConfig;
use crate::services::cache::InMemoryCache;
use crate::services::container::NoopContainerManager;
use crate::services::token::TokenService;
use crate::storage::LocalBlobStorage;

struct Seeded {
    game_id: i32,
    challenge_ids: Vec<i32>,
    users: [Uuid; 2],
}

async fn seed_event(st: &SharedState, pool: &sqlx::PgPool, admin: Uuid) -> Seeded {
    let now = Utc::now();
    let export_game: ExportGameModel = serde_json::from_value(json!({
        "title": "Round Trip Finals",
        "summary": "seeded",
        "startTimeUtc": (now - chrono::Duration::hours(6)).to_rfc3339(),
        "endTimeUtc": (now - chrono::Duration::hours(1)).to_rfc3339(),
        "writeupDeadline": now.to_rfc3339(),
        "divisions": [{ "name": "Open", "defaultPermissions": 2147483647, "challengeConfigs": [] }]
    }))
    .unwrap();
    let challenge = |id: i32, kind: &str, title: &str| {
        serde_json::from_value::<ExportChallengeModel>(json!({
            "id": id,
            "title": title,
            "type": kind,
            "originalScore": 500,
            "minScoreRate": 0.25,
            "difficulty": 5.0,
            "submissionLimit": 0,
            "adScoringWeight": 1.0,
            "networkMode": "Open",
            "containerImage": if kind == "StaticAttachment" { JsonValue::Null } else { json!("registry.test/svc:1") },
            "exposePort": if kind == "StaticAttachment" { JsonValue::Null } else { json!(80) },
            "flags": if kind == "StaticAttachment" { json!([{ "flag": "flag{alpha}" }]) } else { json!([]) }
        }))
        .unwrap()
    };
    let export_challenges = vec![
        challenge(1, "StaticAttachment", "Warmup"),
        challenge(2, "AttackDefense", "Service"),
        challenge(3, "KingOfTheHill", "Hill"),
    ];
    let (definition, ()) = persist_game_import_with(
        st,
        &std::collections::BTreeMap::new(),
        &export_game,
        &export_challenges,
        |_, _| Box::pin(async { Ok(()) }),
    )
    .await
    .unwrap();
    let game_id = definition.game_id;
    let challenge_ids = (1..=3)
        .map(|old| definition.challenge_ids[&old])
        .collect::<Vec<_>>();
    let division_id = definition.division_ids[0];
    let (c1, c2, c3) = (challenge_ids[0], challenge_ids[1], challenge_ids[2]);
    sqlx::query(
        r#"UPDATE "Games"
              SET hidden = FALSE, ad_scoring_start_round = 1, koth_scoring_start_round = 1,
                  ad_epoch_ticks = 2, koth_epoch_ticks = 2, koth_cycle_ticks = 1,
                  koth_champion_cooldown_ticks = 0, koth_claim_confirmation_ticks = 1
            WHERE id = $1"#,
    )
    .bind(game_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(r#"UPDATE "GameChallenges" SET is_enabled = TRUE WHERE game_id = $1"#)
        .bind(game_id)
        .execute(pool)
        .await
        .unwrap();

    let users = [Uuid::new_v4(), Uuid::new_v4()];
    for (index, id) in users.iter().enumerate() {
        sqlx::query(
            r#"INSERT INTO "AspNetUsers"
                 (id, user_name, normalized_user_name, email, normalized_email, email_confirmed,
                  phone_number_confirmed, two_factor_enabled, lockout_enabled, access_failed_count,
                  role, ip, last_signed_in_utc, last_visited_utc, register_time_utc, bio,
                  real_name, std_number, exercise_visible)
               VALUES ($1, $2, upper($2), $3, upper($3), TRUE, FALSE, FALSE, FALSE, 0, 1, '',
                       $4, $4, $4, '', $5, '', TRUE)"#,
        )
        .bind(id)
        .bind(format!("player{index}"))
        .bind(format!("player{index}@example.test"))
        .bind(now)
        .bind(format!("Player {index}"))
        .execute(pool)
        .await
        .unwrap();
    }
    let team_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "Teams" (name, bio, avatar_hash, locked, invite_token, captain_id)
           VALUES ('Alpha', 'first', NULL, TRUE, $1, $2) RETURNING id"#,
    )
    .bind(crate::utils::codec::random_hex(16))
    .bind(users[0])
    .fetch_one(pool)
    .await
    .unwrap();
    for id in users {
        sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES ($1, $2)"#)
            .bind(team_id)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }
    let writeup = st.storage.store("writeup.pdf", b"a writeup").await.unwrap();
    let file_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "Files" (hash, upload_time_utc, file_size, name, reference_count)
           VALUES ($1, $2, 9, 'writeup.pdf', 1) RETURNING id"#,
    )
    .bind(&writeup.hash)
    .bind(now)
    .fetch_one(pool)
    .await
    .unwrap();
    let participation_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "Participations"
             (status, token, writeup_id, game_id, team_id, division_id, suspicion_score)
           VALUES (1, 'source-token', $1, $2, $3, $4, 3) RETURNING id"#,
    )
    .bind(file_id)
    .bind(game_id)
    .bind(team_id)
    .bind(division_id)
    .fetch_one(pool)
    .await
    .unwrap();
    for id in users {
        sqlx::query(
            r#"INSERT INTO "UserParticipations" (user_id, game_id, team_id, participation_id)
               VALUES ($1, $2, $3, $4)"#,
        )
        .bind(id)
        .bind(game_id)
        .bind(team_id)
        .bind(participation_id)
        .execute(pool)
        .await
        .unwrap();
    }

    // Jeopardy records.
    let accepted: i32 = sqlx::query_scalar(
        r#"INSERT INTO "Submissions"
             (answer, status, submit_time_utc, user_id, team_id, participation_id, game_id, challenge_id)
           VALUES ('flag{alpha}', 1, $1, $2, $3, $4, $5, $6) RETURNING id"#,
    )
    .bind(now - chrono::Duration::hours(4))
    .bind(users[0])
    .bind(team_id)
    .bind(participation_id)
    .bind(game_id)
    .bind(c1)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "Submissions"
             (answer, status, submit_time_utc, user_id, team_id, participation_id, game_id, challenge_id)
           VALUES ('flag{nope}', 2, $1, $2, $3, $4, $5, $6)"#,
    )
    .bind(now - chrono::Duration::hours(5))
    .bind(users[1])
    .bind(team_id)
    .bind(participation_id)
    .bind(game_id)
    .bind(c1)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "FirstSolves" (participation_id, challenge_id, submission_id) VALUES ($1, $2, $3)"#)
        .bind(participation_id)
        .bind(c1)
        .bind(accepted)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        r#"UPDATE "GameChallenges" SET accepted_count = 1, submission_count = 2 WHERE id = $1"#,
    )
    .bind(c1)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "GameEvents" (game_id, "Type", values, publish_time_utc, user_id, team_id)
           VALUES ($1, 0, '["Warmup"]', $2, $3, $4)"#,
    )
    .bind(game_id)
    .bind(now - chrono::Duration::hours(4))
    .bind(users[0])
    .bind(team_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "GameNotices" (game_id, "Type", values, publish_time_utc) VALUES ($1, 0, '["hello"]', $2)"#)
        .bind(game_id)
        .bind(now - chrono::Duration::hours(5))
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "ChallengeReviews" (challenge_id, user_id, game_id, rating, comment, submit_time_utc)
           VALUES ($1, $2, $3, 3, 'fine', $4)"#,
    )
    .bind(c1)
    .bind(users[0])
    .bind(game_id)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "AiChatLinks"
             (game_id, participation_id, challenge_id, links, revision, submitted_by, updated_at)
           VALUES ($1, $2, $3, $4, 2, $5, $6)"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .bind(c1)
    .bind(json!([{
        "url": "https://claude.ai/share/2f1c9e4a-8b7d-4c3e-9f60-1a2b3c4d5e6f",
        "providerKey": "claude",
        "providerLabel": "Claude"
    }]))
    .bind(users[1])
    .bind(now - chrono::Duration::hours(2))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "WriteupGrades"
             (game_id, participation_id, challenge_id, percentage, revision, operation_id, graded_by, updated_at)
           VALUES ($1, $2, $3, 80, 1, $4, $5, $6)"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .bind(c1)
    .bind(Uuid::new_v4())
    .bind(admin)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "SuspicionEvents"
             (game_id, participation_id, challenge_id, kind, evidence_key, score_delta, created_at)
           VALUES ($1, $2, $3, 0, 'shared-ip', 3, $4)"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .bind(c1)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();

    // Attack & Defense records.
    let mut rounds = Vec::new();
    for number in 1..=2i32 {
        let round: i32 = sqlx::query_scalar(
            r#"INSERT INTO "AdRounds" (game_id, number, start_time_utc, end_time_utc, finalized)
               VALUES ($1, $2, $3, $4, TRUE) RETURNING id"#,
        )
        .bind(game_id)
        .bind(number)
        .bind(now - chrono::Duration::minutes(180 - 10 * i64::from(number)))
        .bind(now - chrono::Duration::minutes(170 - 10 * i64::from(number)))
        .fetch_one(pool)
        .await
        .unwrap();
        rounds.push(round);
    }
    let service: i32 = sqlx::query_scalar(
        r#"INSERT INTO "AdTeamServices" (game_id, participation_id, challenge_id, host, port, status)
           VALUES ($1, $2, $3, '10.13.40.2', 80, 0) RETURNING id"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .bind(c2)
    .fetch_one(pool)
    .await
    .unwrap();
    let flag: i32 = sqlx::query_scalar(
        r#"INSERT INTO "AdFlags" (round_id, team_service_id, flag, planted_at, checker_qualified, service_weight)
           VALUES ($1, $2, $3, $4, TRUE, 1.0) RETURNING id"#,
    )
    .bind(rounds[0])
    .bind(service)
    .bind(format!("flag{{{}}}", "a".repeat(32)))
    .bind(now - chrono::Duration::minutes(170))
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "AdAttacks" (round_id, attacker_participation_id, victim_team_service_id, flag_id, submitted_at)
           VALUES ($1, $2, $3, $4, $5)"#,
    )
    .bind(rounds[1])
    .bind(participation_id)
    .bind(service)
    .bind(flag)
    .bind(now - chrono::Duration::minutes(159))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "AdCheckResults" (round_id, team_service_id, status, message, checked_at, sla_credit, flag_verified)
           VALUES ($1, $2, 0, 'up', $3, 1.0, TRUE)"#,
    )
    .bind(rounds[0])
    .bind(service)
    .bind(now - chrono::Duration::minutes(169))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "AdEpochRollups"
             (game_id, epoch, start_round, end_round, round_count, epoch_weight, finalized_round,
              eligible_flags, captured_flags, accepted_captures, defense_opportunities,
              protected_opportunities, cumulative_eligible_flags, cumulative_captured_flags,
              cumulative_accepted_captures, cumulative_defense_opportunities,
              cumulative_protected_opportunities)
           VALUES ($1, 1, 1, 2, 2, 1.0, 2, 2, 1, 1, 2, 1, 2, 1, 1, 2, 1)"#,
    )
    .bind(game_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "AdEpochTeamRollups"
             (game_id, epoch, participation_id, points, epoch_weight, cumulative_points_numerator,
              cumulative_epoch_weight, cumulative_offense_numerator, cumulative_defense_numerator,
              cumulative_sla_numerator, cumulative_rate_weight)
           VALUES ($1, 1, $2, 60.0, 1.0, 60.0, 1.0, 0.5, 0.5, 1.0, 1.0)"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "AdEpochServiceRollups"
             (game_id, epoch, participation_id, challenge_id, service_weight, opportunity_count,
              capture_count, rarity_sum, defense_opportunity_count, protected_opportunity_count,
              sla_credit_sum, sla_tick_count, closing_sla_status, closing_sla_credit, local_points,
              offense_rate, defense_rate, sla_rate, cumulative_points_numerator,
              cumulative_epoch_weight, cumulative_offense_numerator, cumulative_defense_numerator,
              cumulative_sla_numerator, cumulative_capture_count)
           VALUES ($1, 1, $2, $3, 1.0, 2, 1, 1.0, 2, 1, 2.0, 2, 0, 1.0, 60.0,
                   0.5, 0.5, 1.0, 60.0, 1.0, 0.5, 0.5, 1.0, 1)"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .bind(c2)
    .execute(pool)
    .await
    .unwrap();

    // King of the Hill records.
    sqlx::query(
        r#"INSERT INTO "KothOfficialConfigs"
             (game_id, scoring_start_round, epoch_ticks, cycle_ticks, champion_cooldown_ticks,
              claim_confirmation_ticks, roster_snapshot, hills_snapshot)
           VALUES ($1, 1, 2, 1, 0, 1, $2, $3)"#,
    )
    .bind(game_id)
    .bind(json!([participation_id]))
    .bind(json!([{ "challengeId": c3, "serviceWeight": 1.0 }]))
    .execute(pool)
    .await
    .unwrap();
    let target: i32 = sqlx::query_scalar(
        r#"INSERT INTO "KothTargets" (game_id, challenge_id, host, port, holder_participation_id, held_since)
           VALUES ($1, $2, '10.13.40.9', 80, $3, $4) RETURNING id"#,
    )
    .bind(game_id)
    .bind(c3)
    .bind(participation_id)
    .bind(now - chrono::Duration::minutes(168))
    .fetch_one(pool)
    .await
    .unwrap();
    let cycle: i64 = sqlx::query_scalar(
        r#"INSERT INTO "KothCrownCycles"
             (game_id, challenge_id, cycle_number, epoch, planned_start_round, planned_end_round,
              actual_start_round, actual_end_round, phase, expected_image, champion_participation_id)
           VALUES ($1, $2, 1, 1, 1, 1, 1, 1, 'Completed', 'registry.test/svc:1', $3) RETURNING id"#,
    )
    .bind(game_id)
    .bind(c3)
    .bind(participation_id)
    .fetch_one(pool)
    .await
    .unwrap();
    let token: i32 = sqlx::query_scalar(
        r#"INSERT INTO "KothTokens"
             (target_id, participation_id, token, submitted_at, round_number, ad_round_id, cycle_id, challenge_id, reset_attempt)
           VALUES ($1, $2, 'crown-token', $3, 1, $4, $5, $6, 0) RETURNING id"#,
    )
    .bind(target)
    .bind(participation_id)
    .bind(now - chrono::Duration::minutes(168))
    .bind(rounds[0])
    .bind(cycle)
    .bind(c3)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "KothControlResults"
             (game_id, challenge_id, ad_round_id, controlling_participation_id, responsible_participation_id,
              marker_observed, status, checked_at, token_window_attempt, cycle_id, container_id,
              token_id, token_window_round, confirmed_participation_id, confirmation_streak,
              is_scorable)
           VALUES ($1, $2, $3, $4, $4, TRUE, 0, $5, 0, $6, 'container-a', $7, 1, $4, 2, TRUE)"#,
    )
    .bind(game_id)
    .bind(c3)
    .bind(rounds[1])
    .bind(participation_id)
    .bind(now - chrono::Duration::minutes(158))
    .bind(cycle)
    .bind(token)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "KothAcquisitions"
             (cycle_id, token_id, game_id, challenge_id, target_id, participation_id, container_id,
              token_window_round, ad_round_id)
           VALUES ($1, $2, $3, $4, $5, $6, 'container-a', 1, $7)"#,
    )
    .bind(cycle)
    .bind(token)
    .bind(game_id)
    .bind(c3)
    .bind(target)
    .bind(participation_id)
    .bind(rounds[0])
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "KothEpochRollups"
             (game_id, epoch, start_round, end_round, round_count, epoch_weight, finalized_round,
              evidence_finalized_at, scorable_ticks, eligible_windows, cumulative_scorable_ticks,
              cumulative_eligible_windows)
           VALUES ($1, 1, 1, 2, 2, 1.0, 2, $2, 2, 1, 2, 1)"#,
    )
    .bind(game_id)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "KothEpochTeamRollups"
             (game_id, epoch, participation_id, points, epoch_weight, acquisition_rate, control_rate,
              sla_rate, acquisition_windows, controlled_ticks, responsible_ticks,
              healthy_responsible_ticks, cumulative_points_numerator, cumulative_epoch_weight,
              cumulative_acquisition_numerator, cumulative_control_numerator,
              cumulative_sla_numerator, cumulative_rate_weight, cumulative_acquisition_windows,
              cumulative_controlled_ticks, cumulative_responsible_ticks,
              cumulative_healthy_responsible_ticks)
           VALUES ($1, 1, $2, 70.0, 1.0, 0.5, 0.5, 1.0, 1, 1, 2, 2, 70.0, 1.0, 0.5, 0.5, 1.0, 1.0, 1, 1, 2, 2)"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "KothEpochHillRollups"
             (game_id, epoch, participation_id, challenge_id, service_weight, evidence_fraction,
              epoch_fraction, local_points, acquisition_rate, control_rate, sla_rate,
              acquisition_windows, controlled_ticks, responsible_ticks, healthy_responsible_ticks,
              cumulative_points_numerator, cumulative_score_weight,
              cumulative_acquisition_numerator, cumulative_control_numerator,
              cumulative_sla_numerator, cumulative_rate_weight, cumulative_acquisition_windows,
              cumulative_controlled_ticks, cumulative_responsible_ticks,
              cumulative_healthy_responsible_ticks)
           VALUES ($1, 1, $2, $3, 1.0, 1.0, 1.0, 70.0, 0.5, 0.5, 1.0, 1, 1, 2, 2, 70.0, 1.0,
                   0.5, 0.5, 1.0, 1.0, 1, 1, 2, 2)"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .bind(c3)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "KothApiScoreResults"
             (game_id, challenge_id, ad_round_id, participation_id, activity_earned, activity_possible,
              objective_earned, objective_possible, objective_count, activity_rate, objective_rate,
              core_rate, performance_rate, lead_credit)
           VALUES ($1, $2, $3, $4, 500000, 1000000, 1000000, 2000000, 2, 0.5, 0.5, 0.5, 0.5, 1.0)"#,
    )
    .bind(game_id)
    .bind(c3)
    .bind(rounds[1])
    .bind(participation_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "ContainerAccessEvents"
             (game_id, challenge_id, container_owner_participation_id, container_id, accessing_user_id,
              accessing_user_name, accessing_participation_id, remote_ip, connected_at_utc)
           VALUES ($1, $2, $3, $4, $5, 'player0', $3, '203.0.113.5', $6)"#,
    )
    .bind(game_id)
    .bind(c2)
    .bind(participation_id)
    .bind(Uuid::new_v4())
    .bind(users[0])
    .bind(now)
    .execute(pool)
    .await
    .unwrap();

    Seeded {
        game_id,
        challenge_ids,
        users,
    }
}

/// Every archived table's rows for one game, as JSON with identifiers and
/// timestamps blanked so two installations of the same event compare equal.
async fn table_snapshot(pool: &sqlx::PgPool, game_id: i32) -> Vec<(String, Vec<JsonValue>)> {
    let mut snapshot = Vec::new();
    for spec in catalog() {
        let rows = sqlx::query_scalar::<_, String>(&spec.select_sql())
            .bind(game_id)
            .bind(10_000i64)
            .bind(0i64)
            .fetch_all(pool)
            .await
            .unwrap_or_else(|error| panic!("{}: {error}", spec.name));
        let rows = rows
            .into_iter()
            .map(|row| {
                let mut value: JsonValue = serde_json::from_str(&row).unwrap();
                blank_identifiers(&mut value);
                value
            })
            .collect();
        snapshot.push((spec.name.to_string(), rows));
    }
    snapshot
}

fn blank_identifiers(value: &mut JsonValue) {
    match value {
        JsonValue::Object(map) => {
            for (key, entry) in map.iter_mut() {
                let is_identifier = key == "id"
                    || key.ends_with("Id")
                    || key.ends_with("Ids")
                    || key == "rosterSnapshot"
                    || key == "hillsSnapshot"
                    || key == "token"
                    || key == "captainId"
                    || key == "competitiveAdmittedAtUtc"
                    || key == "reconciliationVersion";
                if is_identifier {
                    *entry = JsonValue::Null;
                } else {
                    blank_identifiers(entry);
                }
            }
        }
        JsonValue::Array(items) => items.iter_mut().for_each(blank_identifiers),
        _ => {}
    }
}

fn board_summary(value: JsonValue) -> JsonValue {
    let mut value = value;
    blank_identifiers(&mut value);
    if let Some(map) = value.as_object_mut() {
        for volatile in [
            "updateTimeUtc",
            "revision",
            "generatedAtUtc",
            "generatedAt",
            "currentRoundEndsAt",
        ] {
            map.remove(volatile);
        }
    }
    value
}

fn rewrite_archive(bytes: &[u8], rewrite: impl Fn(&str, Vec<u8>) -> Vec<u8>) -> Vec<u8> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let mut content = Vec::new();
        entry.read_to_end(&mut content).unwrap();
        writer.start_file(&name, options).unwrap();
        writer.write_all(&rewrite(&name, content)).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn archive_round_trips_a_multi_format_event_through_the_real_schema() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let application_name = test_process_application_name();
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(
            PgConnectOptions::from_str(&database_url)
                .unwrap()
                .application_name(application_name),
        )
        .await
        .unwrap();
    let schema = format!("rsctf_data_transfer_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin_pool)
        .await
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .connect_with(
            PgConnectOptions::from_str(&database_url)
                .unwrap()
                .application_name(application_name)
                .options([("search_path", schema.as_str())]),
        )
        .await
        .unwrap();
    let db = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
    Migrator::up(&db, None).await.unwrap();
    let storage_root = std::env::temp_dir().join(format!("rsctf-data-transfer-{}", Uuid::new_v4()));
    let st = AppState::new(
        db,
        Arc::new(AppConfig::default()),
        Arc::new(InMemoryCache::new()),
        Arc::new(LocalBlobStorage::new(storage_root.clone())),
        TokenService::new("0123456789abcdef0123456789abcdef", 60),
        Arc::new(NoopContainerManager),
    );
    let admin = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "AspNetUsers"
             (id, user_name, normalized_user_name, email_confirmed, phone_number_confirmed,
              two_factor_enabled, lockout_enabled, access_failed_count, role, ip,
              last_signed_in_utc, last_visited_utc, register_time_utc, bio, real_name, std_number,
              exercise_visible)
           VALUES ($1, 'admin', 'ADMIN', TRUE, FALSE, FALSE, FALSE, 0, 3, '', $2, $2, $2, '', '', '', TRUE)"#,
    )
    .bind(admin)
    .bind(Utc::now())
    .execute(&pool)
    .await
    .unwrap();

    let seeded = seed_event(&st, &pool, admin).await;
    // Arm the keyed identity ledger guards exactly like a bootstrapped
    // installation, so every restored membership and placeholder account must
    // go through the identity-neutral provisioning path.
    sqlx::query(
        r#"INSERT INTO "IdentityObservationBootstrapState"
             (version, key_identifier, completed_at_utc, observations_inserted)
           VALUES (1, decode(repeat('ab', 32), 'hex'), $1, 0)"#,
    )
    .bind(Utc::now())
    .execute(&pool)
    .await
    .unwrap();
    let source_game = game::Entity::find_by_id(seeded.game_id)
        .one(&st.db)
        .await
        .unwrap()
        .unwrap();
    let source_jeopardy =
        crate::controllers::game::build_scoreboard_cached(&st, &source_game, true)
            .await
            .unwrap();
    let source_ad =
        crate::services::ad::scoring::build_ad_scoreboard(&pool, seeded.game_id, true, Utc::now())
            .await
            .unwrap();
    let source_koth =
        crate::controllers::game::koth::build_koth_scoreboard_cached(&st, &source_game, true)
            .await
            .unwrap();
    assert_eq!(source_jeopardy.items[0].score, 500);
    assert_eq!(source_ad.teams[0].team_name, "Alpha");
    assert_eq!(source_koth.teams[0].team_name, "Alpha");

    // --- Export -----------------------------------------------------------
    let bytes = super::export::export_archive_bytes(&st, &source_game, true)
        .await
        .unwrap();
    let source_snapshot = table_snapshot(&pool, seeded.game_id).await;
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes.clone())).unwrap();
    let names = (0..archive.len())
        .map(|index| archive.by_index(index).unwrap().name().to_string())
        .collect::<Vec<_>>();
    for expected in [
        "game.json",
        "manifest.json",
        "scoreboards/jeopardy.json",
        "scoreboards/overall.json",
        "scoreboards/attack-defense.json",
        "scoreboards/koth.json",
        "data/submissions.jsonl",
        "data/adFlags.jsonl",
        "data/kothTokens.jsonl",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "missing {expected}: {names:?}"
        );
    }
    let mut manifest_text = String::new();
    archive
        .by_name("manifest.json")
        .unwrap()
        .read_to_string(&mut manifest_text)
        .unwrap();
    let manifest: DataArchiveManifest = serde_json::from_str(&manifest_text).unwrap();
    assert_eq!(manifest.kind, ARCHIVE_KIND);
    assert_eq!(manifest.source_game_id, seeded.game_id);
    let rows = |name: &str| {
        manifest
            .tables
            .iter()
            .find(|table| table.name == name)
            .map(|table| table.rows)
            .unwrap()
    };
    assert_eq!(rows("users"), 3, "members, captain, and the grading admin");
    assert_eq!(rows("submissions"), 2);
    assert_eq!(rows("firstSolves"), 1);
    assert_eq!(rows("adRounds"), 2);
    assert_eq!(rows("adFlags"), 1);
    assert_eq!(rows("kothTokens"), 1);
    assert_eq!(rows("writeups"), 1);
    assert_eq!(rows("kothOfficialConfigs"), 1);
    assert_eq!(rows("aiChatLinks"), 1);
    let mut users_text = String::new();
    archive
        .by_name("data/users.jsonl")
        .unwrap()
        .read_to_string(&mut users_text)
        .unwrap();
    assert!(!users_text.contains("password") && !users_text.contains("securityStamp"));
    let writeup_entry = names
        .iter()
        .filter(|name| name.starts_with("files/") && !name.ends_with('/'))
        .count();
    assert_eq!(writeup_entry, 1, "the writeup blob is bundled");

    let unbundled = super::export::export_archive_bytes(&st, &source_game, false)
        .await
        .unwrap();
    let mut unbundled_archive = zip::ZipArchive::new(Cursor::new(unbundled)).unwrap();
    let mut unbundled_manifest = String::new();
    unbundled_archive
        .by_name("manifest.json")
        .unwrap()
        .read_to_string(&mut unbundled_manifest)
        .unwrap();
    let unbundled_manifest: DataArchiveManifest =
        serde_json::from_str(&unbundled_manifest).unwrap();
    assert!(!unbundled_manifest.attachments_bundled);
    assert!(manifest.attachments_bundled);

    // --- Restore on the same installation ---------------------------------
    let result = super::import::restore_archive(&st, bytes.clone(), admin)
        .await
        .unwrap();
    assert_ne!(result.game_id, seeded.game_id);
    assert_eq!(result.users.matched, 3);
    assert_eq!(result.users.created, 0);
    assert_eq!(result.teams.matched, 1);
    assert_eq!(result.teams.created, 0);
    for table in &result.tables {
        let archived = rows(&table.name);
        let restorable = catalog()
            .find(|spec| spec.name == table.name)
            .map(|spec| !matches!(spec.restore, Restore::ExportOnly))
            .unwrap();
        assert_eq!(table.restored, restorable, "{}", table.name);
        assert_eq!(table.rows, archived, "{} restored count", table.name);
    }
    let restored_game = game::Entity::find_by_id(result.game_id)
        .one(&st.db)
        .await
        .unwrap()
        .unwrap();
    assert!(restored_game.hidden);
    assert_eq!(restored_game.title, "Round Trip Finals");
    let restored_snapshot = table_snapshot(&pool, result.game_id).await;
    for ((name, source_rows), (_, restored_rows)) in source_snapshot.iter().zip(&restored_snapshot)
    {
        let restorable = catalog()
            .find(|spec| spec.name == name)
            .map(|spec| !matches!(spec.restore, Restore::ExportOnly))
            .unwrap();
        if restorable {
            assert_eq!(
                source_rows, restored_rows,
                "table {name} differs after restore"
            );
        }
    }
    let participation_tokens: Vec<String> =
        sqlx::query_scalar(r#"SELECT token FROM "Participations" WHERE game_id = $1"#)
            .bind(result.game_id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(participation_tokens.len(), 1);
    assert_ne!(participation_tokens[0], "source-token");
    let restored_writeup: Option<i32> =
        sqlx::query_scalar(r#"SELECT writeup_id FROM "Participations" WHERE game_id = $1"#)
            .bind(result.game_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        restored_writeup.is_some(),
        "writeup re-attached from the bundle"
    );
    let counts: (i32, i32) = sqlx::query_as(
        r#"SELECT accepted_count, submission_count FROM "GameChallenges"
            WHERE game_id = $1 ORDER BY id LIMIT 1"#,
    )
    .bind(result.game_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counts, (1, 2), "counters recomputed from restored rows");

    let restored_jeopardy =
        crate::controllers::game::build_scoreboard_cached(&st, &restored_game, true)
            .await
            .unwrap();
    assert_eq!(
        board_summary(serde_json::to_value(&restored_jeopardy).unwrap()),
        board_summary(serde_json::to_value(&source_jeopardy).unwrap()),
    );
    let restored_ad =
        crate::services::ad::scoring::build_ad_scoreboard(&pool, result.game_id, true, Utc::now())
            .await
            .unwrap();
    assert_eq!(
        board_summary(serde_json::to_value(&restored_ad).unwrap()),
        board_summary(serde_json::to_value(&source_ad).unwrap()),
    );
    let restored_koth =
        crate::controllers::game::koth::build_koth_scoreboard_cached(&st, &restored_game, true)
            .await
            .unwrap();
    assert_eq!(
        board_summary(serde_json::to_value(&restored_koth).unwrap()),
        board_summary(serde_json::to_value(&source_koth).unwrap()),
    );

    // --- Restore on a "different installation": unknown users ---------------
    let stranger = Uuid::new_v4();
    let relocated = rewrite_archive(&bytes, |name, content| {
        if name == "data/users.jsonl"
            || name == "data/aiChatLinks.jsonl"
            || name == "data/teamMembers.jsonl"
            || name == "data/userParticipations.jsonl"
            || name == "data/submissions.jsonl"
            || name == "data/gameEvents.jsonl"
            || name == "data/challengeReviews.jsonl"
            || name == "data/containerAccessEvents.jsonl"
            || name == "data/teams.jsonl"
        {
            let text = String::from_utf8(content).unwrap();
            text.replace(&seeded.users[1].to_string(), &stranger.to_string())
                .replace("player1@example.test", "stranger@example.test")
                .replace("\"player1\"", "\"stranger\"")
                .into_bytes()
        } else {
            content
        }
    });
    let games_before: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "Games""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    let relocated_result = super::import::restore_archive(&st, relocated, admin)
        .await
        .unwrap();
    assert_eq!(relocated_result.users.created, 1);
    assert_eq!(relocated_result.users.matched, 2);
    let placeholder: (Option<String>, String, bool) = sqlx::query_as(
        r#"SELECT password_hash, user_name, email_confirmed FROM "AspNetUsers" WHERE id = $1"#,
    )
    .bind(stranger)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(placeholder, (None, "stranger".to_string(), false));

    // --- Rejections leave nothing behind ------------------------------------
    let games_after: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "Games""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(games_after, games_before + 1);
    let dangling = rewrite_archive(&bytes, |name, content| {
        if name == "data/firstSolves.jsonl" {
            b"{\"participationId\":1,\"challengeId\":1,\"submissionId\":999999}\n".to_vec()
        } else {
            content
        }
    });
    let error = super::import::restore_archive(&st, dangling, admin)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("unknown"), "{error}");
    let unfinished = rewrite_archive(&bytes, |name, content| {
        if name == "game.json" {
            let mut value: JsonValue = serde_json::from_slice(&content).unwrap();
            value["endTimeUtc"] = json!((Utc::now() + chrono::Duration::hours(1)).to_rfc3339());
            value.to_string().into_bytes()
        } else {
            content
        }
    });
    let error = super::import::restore_archive(&st, unfinished, admin)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("end time"), "{error}");
    let not_an_archive = rewrite_archive(&bytes, |name, content| {
        if name == "manifest.json" {
            b"{\"kind\":\"rsctf-game\",\"formatVersion\":1,\"exportedAtUtc\":0,\"sourceGameId\":1,\"title\":\"x\",\"platformVersion\":\"0\",\"tables\":[]}".to_vec()
        } else {
            content
        }
    });
    assert!(super::import::restore_archive(&st, not_an_archive, admin)
        .await
        .is_err());
    let games_final: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "Games""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(games_final, games_after, "rejected restores create no game");
    assert_eq!(seeded.challenge_ids.len(), 3);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin_pool)
        .await
        .unwrap();
    admin_pool.close().await;
    let _ = std::fs::remove_dir_all(storage_root);
}
