use sea_orm::SqlxPostgresConnector;
use sea_orm_migration::MigratorTrait;
use sqlx::postgres::PgPoolOptions;

pub(super) struct AdScoringFixture {
    pub pool: sqlx::PgPool,
    pub game_id: i32,
    admin_pool: sqlx::PgPool,
    schema: String,
}

impl AdScoringFixture {
    pub(super) async fn cleanup(self) {
        self.pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
            .execute(&self.admin_pool)
            .await
            .expect("drop isolated A&D fixture schema");
        self.admin_pool.close().await;
    }
}

/// Build one isolated, fully migrated A&D event for the database scoring
/// regressions without relying on a developer's pre-provisioned game. Each
/// Tokio test owns its pool because a pool cannot outlive the runtime that
/// created it.
pub(super) async fn ad_scoring_fixture() -> AdScoringFixture {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin_options = crate::migrations::test_pg_connect_options(&database_url);
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(admin_options)
        .await
        .expect("connect A&D fixture admin pool");
    let schema = format!("rsctf_ad_scoring_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin_pool)
        .await
        .expect("create isolated A&D fixture schema");
    let options = crate::migrations::test_pg_connect_options(&database_url)
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .expect("connect isolated A&D fixture pool");
    let database = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
    crate::migrations::Migrator::up(&database, None)
        .await
        .expect("migrate isolated A&D fixture schema");

    seed_complete_epoch(&pool).await;
    AdScoringFixture {
        pool,
        game_id: 900_001,
        admin_pool,
        schema,
    }
}

async fn seed_complete_epoch(pool: &sqlx::PgPool) {
    sqlx::raw_sql(
        r#"
        INSERT INTO "AspNetUsers" (
          id, user_name, normalized_user_name, email, normalized_email,
          email_confirmed, phone_number_confirmed, two_factor_enabled,
          lockout_enabled, access_failed_count, role, ip,
          last_signed_in_utc, last_visited_utc, register_time_utc,
          bio, real_name, std_number, exercise_visible
        ) VALUES
          ('00000000-0000-0000-0000-000000000011', 'fixture-one',
           'FIXTURE-ONE', 'one@example.test', 'ONE@EXAMPLE.TEST',
           TRUE, FALSE, FALSE, FALSE, 0, 0, '127.0.0.1',
           clock_timestamp(), clock_timestamp(), clock_timestamp(), '', '', '', FALSE),
          ('00000000-0000-0000-0000-000000000012', 'fixture-two',
           'FIXTURE-TWO', 'two@example.test', 'TWO@EXAMPLE.TEST',
           TRUE, FALSE, FALSE, FALSE, 0, 0, '127.0.0.1',
           clock_timestamp(), clock_timestamp(), clock_timestamp(), '', '', '', FALSE);

        INSERT INTO "Teams" (
          id, name, locked, invite_token, captain_id
        ) VALUES
          (900011, 'Fixture One', FALSE, 'fixture-team-one',
           '00000000-0000-0000-0000-000000000011'),
          (900012, 'Fixture Two', FALSE, 'fixture-team-two',
           '00000000-0000-0000-0000-000000000012');

        INSERT INTO "Games" (
          id, title, public_key, private_key, hidden, practice_mode,
          summary, content, accept_without_review, allow_user_submissions,
          writeup_required, team_member_count_limit, container_count_limit,
          start_time_utc, end_time_utc, writeup_deadline, writeup_note,
          blood_bonus_value, ad_allow_snapshot_download, ad_scoring_paused,
          ad_epoch_ticks, ad_scoring_start_round,
          koth_epoch_ticks, koth_cycle_ticks,
          koth_champion_cooldown_ticks, koth_claim_confirmation_ticks
        ) VALUES (
          900001, 'A&D scoring regression', 'fixture-public', 'fixture-private',
          FALSE, FALSE, '', '', TRUE, FALSE, FALSE, 0, 0,
          clock_timestamp() - INTERVAL '2 hours',
          clock_timestamp() - INTERVAL '1 minute',
          clock_timestamp() + INTERVAL '1 day', '', 0,
          FALSE, FALSE, 8, 1, 12, 3, 1, 2
        );

        INSERT INTO "Participations" (
          id, status, token, game_id, team_id, suspicion_score
        ) VALUES
          (900021, 1, 'fixture-participation-one', 900001, 900011, 0),
          (900022, 1, 'fixture-participation-two', 900001, 900012, 0);

        INSERT INTO "GameChallenges" (
          id, game_id, title, content, category, "Type", is_enabled,
          submission_limit, accepted_count, submission_count, review_status,
          build_status, enable_traffic_capture, enable_shared_container,
          disable_blood_bonus, original_score, min_score_rate, difficulty,
          score_curve, ad_allow_egress, ad_allow_self_reset,
          ad_ssh_requires_flag, ad_self_hosted, ad_scoring_weight
        ) VALUES (
          900031, 900001, 'Fixture service', '', 2, 4, TRUE,
          0, 0, 0, 0, 0, FALSE, FALSE, FALSE, 1000, 0.25, 5.0,
          0, FALSE, FALSE, FALSE, TRUE, 1.0
        );

        INSERT INTO "AdTeamServices" (
          id, game_id, participation_id, challenge_id, host, port, status
        ) VALUES
          (900041, 900001, 900021, 900031, 'fixture-one', 31337, 0),
          (900042, 900001, 900022, 900031, 'fixture-two', 31337, 0);

        INSERT INTO "AdRounds" (
          id, game_id, number, start_time_utc, end_time_utc, finalized,
          pipeline_completed_at, flags_published_at
        )
        SELECT 900100 + number, 900001, number,
               clock_timestamp() - INTERVAL '40 minutes'
                 + number * INTERVAL '1 minute',
               clock_timestamp() - INTERVAL '39 minutes'
                 + number * INTERVAL '1 minute',
               TRUE,
               clock_timestamp() - INTERVAL '39 minutes'
                 + number * INTERVAL '1 minute',
               clock_timestamp() - INTERVAL '40 minutes'
                 + number * INTERVAL '1 minute'
          FROM generate_series(1, 8) AS number;

        INSERT INTO "AdFlags" (
          id, round_id, team_service_id, flag, planted_at,
          checker_qualified, service_weight
        )
        SELECT 901000 + round.number * 10 + service.ordinal,
               round.id, service.id,
               'flag{' || round.number || '-' || service.ordinal || '}',
               round.start_time_utc, TRUE, 1.0
          FROM "AdRounds" round
          CROSS JOIN (VALUES (900041, 1), (900042, 2)) service(id, ordinal)
         WHERE round.game_id = 900001;

        INSERT INTO "AdCheckResults" (
          id, round_id, team_service_id, status, checked_at,
          sla_credit, flag_verified
        )
        SELECT 902000 + round.number * 10 + service.ordinal,
               round.id, service.id, 0,
               round.start_time_utc + INTERVAL '2 seconds', 1.0, TRUE
          FROM "AdRounds" round
          CROSS JOIN (VALUES (900041, 1), (900042, 2)) service(id, ordinal)
         WHERE round.game_id = 900001;

        INSERT INTO "AdFlagDeliveryResults" (
          round_id, team_service_id, delivery_kind, delivered, attempts,
          completed_at
        )
        SELECT round.id, service.id, 'External', TRUE, 1,
               round.start_time_utc + INTERVAL '1 second'
          FROM "AdRounds" round
          CROSS JOIN (VALUES (900041), (900042)) service(id)
         WHERE round.game_id = 900001;

        INSERT INTO "AdAttacks" (
          round_id, attacker_participation_id, victim_team_service_id,
          flag_id, submitted_at
        )
        SELECT flag.round_id, 900021, 900042, flag.id,
               round.start_time_utc + INTERVAL '10 seconds'
          FROM "AdFlags" flag
          JOIN "AdRounds" round ON round.id = flag.round_id
         WHERE flag.team_service_id = 900042 AND round.number = 1;
        "#,
    )
    .execute(pool)
    .await
    .expect("seed complete A&D scoring epoch");
}
