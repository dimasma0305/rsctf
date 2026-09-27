//! Seeds a multi-format event (Jeopardy, A&D, KotH, evidence, AI chat
//! disclosure) for the competition data archive round trip.

use super::*;

pub(super) struct Seeded {
    pub(super) game_id: i32,
    pub(super) challenge_ids: Vec<i32>,
    pub(super) users: [Uuid; 2],
}

pub(super) async fn seed_event(st: &SharedState, pool: &sqlx::PgPool, admin: Uuid) -> Seeded {
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
        r#"INSERT INTO "AiChatLinkEvents"
             (game_id, participation_id, challenge_id, user_id, action, revision,
              previous_links, links, added_urls, removed_urls, previous_declared_no_ai,
              declared_no_ai, solved_at, seconds_since_solve, remote_ip_hash, occurred_at)
           VALUES ($1, $2, $3, $4, 'Edited', 2, '[]', $5, $6, '[]', TRUE, FALSE, $7, 3600,
                   decode(repeat('ab', 32), 'hex'), $8)"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .bind(c1)
    .bind(users[1])
    .bind(json!([{
        "url": "https://claude.ai/share/2f1c9e4a-8b7d-4c3e-9f60-1a2b3c4d5e6f",
        "providerKey": "claude",
        "providerLabel": "Claude"
    }]))
    .bind(json!([
        "https://claude.ai/share/2f1c9e4a-8b7d-4c3e-9f60-1a2b3c4d5e6f"
    ]))
    .bind(now - chrono::Duration::hours(4))
    .bind(now - chrono::Duration::hours(3))
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
