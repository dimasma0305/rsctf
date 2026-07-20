use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::{mark_team_participations_revoked, remove_membership, TeamDeletionLease};

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn deletion_authorization_survives_start_time_but_not_late_evidence() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("team_delete_retry_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TABLE "Games" (
          id INTEGER PRIMARY KEY,
          start_time_utc TIMESTAMPTZ NOT NULL,
          deletion_pending BOOLEAN NOT NULL DEFAULT FALSE,
          ad_scoring_start_round INTEGER,
          koth_scoring_start_round INTEGER
        );
        CREATE TABLE "Teams" (
          id INTEGER PRIMARY KEY,
          deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
        );
        CREATE TABLE "Participations" (
          id INTEGER PRIMARY KEY,
          game_id INTEGER NOT NULL,
          team_id INTEGER NOT NULL,
          status SMALLINT NOT NULL,
          writeup_id INTEGER
        );
        CREATE TABLE "TeamMembers" (team_id INTEGER NOT NULL);
        CREATE TABLE "UserParticipations" (team_id INTEGER NOT NULL);
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    crate::services::participation_evidence::create_test_evidence_tables(&pool)
        .await
        .unwrap();

    let base = (uuid::Uuid::new_v4().as_u128() % 900_000_000) as i32 + 1;
    for offset in 0..=2 {
        let game_id = base + offset;
        let team_id = base + 10 + offset;
        let participation_id = base + 20 + offset;
        sqlx::query(
            r#"INSERT INTO "Games"
                 (id, start_time_utc, ad_scoring_start_round,
                  koth_scoring_start_round)
               VALUES ($1, clock_timestamp() + interval '1 hour', NULL, NULL)"#,
        )
        .bind(game_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(r#"INSERT INTO "Teams" (id) VALUES ($1)"#)
            .bind(team_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "Participations" (id, game_id, team_id, status)
               VALUES ($1, $2, $3, 1)"#,
        )
        .bind(participation_id)
        .bind(game_id)
        .bind(team_id)
        .execute(&pool)
        .await
        .unwrap();

        let key = format!("{schema}:team-roster:{team_id}");
        let mut authorization = crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, &key)
            .await
            .unwrap();
        mark_team_participations_revoked(&mut authorization, team_id)
            .await
            .unwrap();
        authorization.release().await.unwrap();
        sqlx::query(
            r#"UPDATE "Games"
                  SET start_time_utc = clock_timestamp() - interval '1 microsecond'
                WHERE id = $1"#,
        )
        .bind(game_id)
        .execute(&pool)
        .await
        .unwrap();
    }

    let clean_team = base + 10;
    let clean_key = format!("{schema}:team-roster:{clean_team}");
    let mut retry = crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, &clean_key)
        .await
        .unwrap();
    mark_team_participations_revoked(&mut retry, clean_team)
        .await
        .expect("wall-clock start invalidated a durable team deletion fence");
    retry.release().await.unwrap();
    TeamDeletionLease::acquire(&pool, &clean_key, clean_team)
        .await
        .unwrap()
        .unwrap()
        .finalize(clean_team)
        .await
        .expect("finalization rejected only because the wall clock crossed start");

    let evidence_team = base + 11;
    let evidence_participation = base + 21;
    sqlx::query(r#"INSERT INTO "HoneypotHits" (participation_id) VALUES ($1)"#)
        .bind(evidence_participation)
        .execute(&pool)
        .await
        .unwrap();
    let evidence_key = format!("{schema}:team-roster:{evidence_team}");
    let mut retry = crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, &evidence_key)
        .await
        .unwrap();
    let retry_error = mark_team_participations_revoked(&mut retry, evidence_team)
        .await
        .expect_err("retry ignored evidence recorded after authorization");
    assert_eq!(retry_error.status(), axum::http::StatusCode::BAD_REQUEST);
    drop(retry);
    let final_error = TeamDeletionLease::acquire(&pool, &evidence_key, evidence_team)
        .await
        .unwrap()
        .unwrap()
        .finalize(evidence_team)
        .await
        .expect_err("finalization erased evidence recorded after authorization");
    assert_eq!(final_error.status(), axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "Teams" WHERE id = $1"#)
            .bind(evidence_team)
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );

    let delayed_game = base + 2;
    let delayed_team = base + 12;
    let delayed_participation = base + 22;
    let delayed_key = format!("{schema}:team-roster:{delayed_team}");
    // This branch exercises the first authorization commit, not the retry
    // state prepared by the loop above. Restore a live row so the delayed
    // writer must wait on the participation lock and then recheck the newly
    // committed suspension/deletion marker.
    sqlx::query(
        r#"UPDATE "Games"
              SET start_time_utc = clock_timestamp() + interval '1 hour'
            WHERE id = $1"#,
    )
    .bind(delayed_game)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"UPDATE "Teams" SET deletion_pending = FALSE WHERE id = $1"#)
        .bind(delayed_team)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "Participations" SET status = 1 WHERE id = $1"#)
        .bind(delayed_participation)
        .execute(&pool)
        .await
        .unwrap();
    let mut deletion = crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, &delayed_key)
        .await
        .unwrap();
    mark_team_participations_revoked(&mut deletion, delayed_team)
        .await
        .unwrap();
    let mut late_writer = tokio::spawn({
        let pool = pool.clone();
        async move {
            let mut transaction = pool.begin().await.unwrap();
            let eligible = crate::services::participation_evidence::lock_audit_insert_scope(
                &mut transaction,
                delayed_game,
                None,
                &[delayed_participation],
            )
            .await
            .unwrap();
            if eligible {
                sqlx::query(r#"INSERT INTO "HoneypotHits" (participation_id) VALUES ($1)"#)
                    .bind(delayed_participation)
                    .execute(&mut *transaction)
                    .await
                    .unwrap();
            }
            transaction.commit().await.unwrap();
            eligible
        }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut late_writer)
            .await
            .is_err(),
        "late writer crossed the uncommitted team deletion marker"
    );
    deletion.release().await.unwrap();
    assert!(
        !tokio::time::timeout(std::time::Duration::from_secs(2), late_writer)
            .await
            .unwrap()
            .unwrap(),
        "late writer attributed evidence after team deletion was fenced"
    );
    let mut retry = crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, &delayed_key)
        .await
        .unwrap();
    mark_team_participations_revoked(&mut retry, delayed_team)
        .await
        .expect("blocked audit writer poisoned the authorized team retry");
    retry.release().await.unwrap();
    TeamDeletionLease::acquire(&pool, &delayed_key, delayed_team)
        .await
        .unwrap()
        .unwrap()
        .finalize(delayed_team)
        .await
        .unwrap();

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn roster_removal_retains_scoring_attribution_and_blocks_physical_user_deletion() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("team_history_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TABLE "AspNetUsers" (
          id UUID PRIMARY KEY,
          role SMALLINT NOT NULL,
          security_stamp TEXT
        );
        CREATE TABLE "Teams" (
          id INTEGER PRIMARY KEY,
          captain_id UUID NOT NULL
        );
        CREATE TABLE "TeamMembers" (
          team_id INTEGER NOT NULL,
          user_id UUID NOT NULL
        );
        CREATE TABLE "Games" (
          id INTEGER PRIMARY KEY,
          end_time_utc TIMESTAMPTZ NOT NULL
        );
        CREATE TABLE "Participations" (
          id INTEGER PRIMARY KEY,
          status SMALLINT NOT NULL,
          game_id INTEGER NOT NULL,
          writeup_id INTEGER
        );
        CREATE TABLE "UserParticipations" (
          user_id UUID NOT NULL,
          game_id INTEGER NOT NULL,
          team_id INTEGER NOT NULL,
          participation_id INTEGER NOT NULL,
          PRIMARY KEY (user_id, game_id)
        );
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    crate::services::participation_evidence::create_test_evidence_tables(&pool)
        .await
        .unwrap();

    let captain_id = uuid::Uuid::new_v4();
    let accepted_user = uuid::Uuid::new_v4();
    let evidence_user = uuid::Uuid::new_v4();
    let mutable_user = uuid::Uuid::new_v4();
    let active_accepted_user = uuid::Uuid::new_v4();
    for user_id in [
        accepted_user,
        evidence_user,
        mutable_user,
        active_accepted_user,
    ] {
        sqlx::query(
            r#"INSERT INTO "AspNetUsers" (id, role, security_stamp)
               VALUES ($1, $2, 'unchanged')"#,
        )
        .bind(user_id)
        .bind(crate::utils::enums::Role::User as i16)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(r#"INSERT INTO "Teams" (id, captain_id) VALUES (41, $1)"#)
        .bind(captain_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "Games" (id, end_time_utc)
           VALUES (61, clock_timestamp() - interval '1 hour'),
                  (62, clock_timestamp() + interval '1 hour')"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let fixtures = [
        (
            accepted_user,
            51,
            crate::utils::enums::ParticipationStatus::Accepted,
            61,
        ),
        (
            evidence_user,
            52,
            crate::utils::enums::ParticipationStatus::Rejected,
            61,
        ),
        (
            mutable_user,
            53,
            crate::utils::enums::ParticipationStatus::Pending,
            61,
        ),
        (
            active_accepted_user,
            54,
            crate::utils::enums::ParticipationStatus::Accepted,
            62,
        ),
    ];
    for (user_id, participation_id, status, game_id) in fixtures {
        sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES (41, $1)"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "Participations" (id, status, game_id, writeup_id)
               VALUES ($1, $2, $3, NULL)"#,
        )
        .bind(participation_id)
        .bind(status as i16)
        .bind(game_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "UserParticipations"
                 (user_id, game_id, team_id, participation_id)
               VALUES ($1, $2, 41, $3)"#,
        )
        .bind(user_id)
        .bind(game_id)
        .bind(participation_id)
        .execute(&pool)
        .await
        .unwrap();
    }
    // Both immutable paths are intentional: a completed Accepted/Suspended
    // row is the historical roster identity even with zero points, while
    // evidence also protects a malformed legacy row later set to Rejected.
    sqlx::query(r#"INSERT INTO "Submissions" (participation_id) VALUES (52)"#)
        .execute(&pool)
        .await
        .unwrap();

    let mut transaction = pool.begin().await.unwrap();
    for (user_id, _, _, _) in fixtures {
        remove_membership(&mut transaction, 41, user_id)
            .await
            .unwrap();
    }
    transaction.commit().await.unwrap();

    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "TeamMembers""#)
            .fetch_one(&pool)
            .await
            .unwrap(),
        0,
        "leave/kick did not revoke the live team roster"
    );
    for user_id in [accepted_user, evidence_user] {
        assert!(
            sqlx::query_scalar::<_, bool>(
                r#"SELECT EXISTS(
                     SELECT 1 FROM "UserParticipations" WHERE user_id = $1
                   )"#,
            )
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            "roster removal erased historical scoring attribution"
        );
        let error = crate::controllers::admin::fence_user_for_deletion(&pool, user_id)
            .await
            .expect_err("historically attributed user passed the physical-delete fence");
        assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(
            error.to_string(),
            "Cannot delete a user who belongs to a team"
        );
    }
    for user_id in [mutable_user, active_accepted_user] {
        assert!(
            !sqlx::query_scalar::<_, bool>(
                r#"SELECT EXISTS(
                     SELECT 1 FROM "UserParticipations" WHERE user_id = $1
                   )"#,
            )
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            "live unscored game link survived roster removal"
        );
    }

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
