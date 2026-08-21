use std::time::Duration as StdDuration;

use chrono::{Duration, Utc};
use uuid::Uuid;

use super::db_tests::{insert_observation, insert_user, set_policy, test_config, Harness};
use super::*;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn historical_collision_does_not_overreach_into_a_clean_target_roster() {
    let harness = Harness::new().await;
    let config = test_config();
    let shared_captain = Uuid::new_v4();
    let joining = Uuid::new_v4();
    let target_captain = Uuid::new_v4();
    for (id, name) in [
        (shared_captain, "shared"),
        (joining, "joining"),
        (target_captain, "target"),
    ] {
        insert_user(&harness.pool, id, name, "0.0.0.0").await;
    }
    sqlx::query(r#"INSERT INTO "Teams" (id,captain_id) VALUES (20,$1),(10,$2)"#)
        .bind(shared_captain)
        .bind(target_captain)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "TeamMembers" (team_id,user_id) VALUES (20,$1)"#)
        .bind(joining)
        .execute(&harness.pool)
        .await
        .unwrap();

    let old_collision = prepare_identity(
        config.identity_hash_key.as_bytes(),
        Some("192.0.2.50"),
        None,
    );
    for user_id in [shared_captain, joining] {
        insert_observation(
            &harness.pool,
            user_id,
            None,
            None,
            None,
            &old_collision.values[0],
            Utc::now(),
        )
        .await;
    }
    let target_identity = prepare_identity(
        config.identity_hash_key.as_bytes(),
        Some("203.0.113.90"),
        None,
    );
    insert_observation(
        &harness.pool,
        target_captain,
        None,
        None,
        None,
        &target_identity.values[0],
        Utc::now(),
    )
    .await;
    // Simulate enabling the per-team policy after the historic shared-team
    // collision. A clean current identity joining a clean target is allowed.
    set_policy(
        &harness.pool,
        PolicyFlags {
            require_unique_ip_per_team_user: true,
            ..Default::default()
        },
    )
    .await;
    let mut transaction = harness.pool.begin().await.unwrap();
    let outcome = super::roster::admit_team_member_in_transaction(
        &mut transaction,
        &config,
        joining,
        Some("joining"),
        10,
        Some("198.51.100.90"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(outcome, AdmissionOutcome::Accepted);
    transaction.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn canonical_policy_requires_or_discards_fingerprint_after_policy_flip() {
    let harness = Harness::new().await;
    let config = test_config();
    let user_id = Uuid::new_v4();
    insert_user(&harness.pool, user_id, "fingerprint-user", "192.0.2.1").await;
    set_policy(
        &harness.pool,
        PolicyFlags {
            enable_browser_fingerprint: true,
            ..Default::default()
        },
    )
    .await;
    assert!(admit_existing_user(
        &harness.pool,
        &config,
        user_id,
        Some("fingerprint-user"),
        Some("192.0.2.2"),
        None,
        IdentitySource::Password,
        "stamp",
        None,
        crate::services::captcha::CaptchaAdmission::Local(None),
    )
    .await
    .is_err());

    set_policy(&harness.pool, PolicyFlags::default()).await;
    assert_eq!(
        admit_existing_user(
            &harness.pool,
            &config,
            user_id,
            Some("fingerprint-user"),
            Some("192.0.2.2"),
            Some(&"a".repeat(64)),
            IdentitySource::Password,
            "stamp",
            None,
            crate::services::captcha::CaptchaAdmission::Local(None),
        )
        .await
        .unwrap(),
        AdmissionOutcome::Accepted
    );
    let fingerprint_rows: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM "IdentityObservations"
            WHERE user_id=$1 AND kind='Fingerprint'"#,
    )
    .bind(user_id)
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(fingerprint_rows, 0);
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn evidence_closed_game_receives_no_scoped_identity_observation() {
    let harness = Harness::new().await;
    let config = test_config();
    let user_id = Uuid::new_v4();
    insert_user(&harness.pool, user_id, "closed-game-user", "192.0.2.1").await;
    let now = Utc::now();
    sqlx::query(
        r#"INSERT INTO "Games" (id,start_time_utc,end_time_utc)
           VALUES (1,$1,$2)"#,
    )
    .bind(now - Duration::hours(1))
    .bind(now + Duration::hours(1))
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "Participations" (id,game_id,team_id,status)
           VALUES (1,1,10,$1)"#,
    )
    .bind(crate::utils::enums::ParticipationStatus::Accepted as i16)
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "UserParticipations"
             (user_id,game_id,team_id,participation_id)
           VALUES ($1,1,10,1)"#,
    )
    .bind(user_id)
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "SuspicionReconciliationState"
             (game_id,evidence_closed_at_utc) VALUES (1,clock_timestamp())"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();

    assert_eq!(
        admit_existing_user(
            &harness.pool,
            &config,
            user_id,
            Some("closed-game-user"),
            Some("198.51.100.22"),
            None,
            IdentitySource::Password,
            "stamp",
            None,
            crate::services::captcha::CaptchaAdmission::Local(None),
        )
        .await
        .unwrap(),
        AdmissionOutcome::Accepted
    );
    let (global, scoped): (i64, i64) = sqlx::query_as(
        r#"SELECT
             COUNT(*) FILTER (WHERE game_id IS NULL),
             COUNT(*) FILTER (WHERE game_id = 1)
           FROM "IdentityObservations" WHERE user_id=$1"#,
    )
    .bind(user_id)
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!((global, scoped), (1, 0));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn game_start_edit_while_observation_waits_is_rechecked_after_game_lock() {
    let harness = Harness::new().await;
    let config = test_config();
    let user_id = Uuid::new_v4();
    insert_user(&harness.pool, user_id, "boundary-user", "192.0.2.1").await;
    let now = Utc::now();
    sqlx::query(r#"INSERT INTO "Teams" (id,captain_id) VALUES (10,$1)"#)
        .bind(user_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "Games" (id,start_time_utc,end_time_utc)
           VALUES (1,$1,$2)"#,
    )
    .bind(now + Duration::hours(1))
    .bind(now + Duration::hours(2))
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "Participations" (id,game_id,team_id,status)
           VALUES (1,1,10,$1)"#,
    )
    .bind(crate::utils::enums::ParticipationStatus::Accepted as i16)
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "UserParticipations"
             (user_id,game_id,team_id,participation_id)
           VALUES ($1,1,10,1)"#,
    )
    .bind(user_id)
    .execute(&harness.pool)
    .await
    .unwrap();

    let mut editor = harness.pool.begin().await.unwrap();
    sqlx::query(
        r#"UPDATE "Games"
              SET start_time_utc = clock_timestamp() - INTERVAL '1 hour'
            WHERE id = 1"#,
    )
    .execute(&mut *editor)
    .await
    .unwrap();
    let pool = harness.pool.clone();
    let admission = tokio::spawn(async move {
        admit_existing_user(
            &pool,
            &config,
            user_id,
            Some("boundary-user"),
            Some("198.51.100.23"),
            None,
            IdentitySource::Password,
            "stamp",
            None,
            crate::services::captcha::CaptchaAdmission::Local(None),
        )
        .await
    });
    tokio::time::sleep(StdDuration::from_millis(25)).await;
    assert!(
        !admission.is_finished(),
        "admission skipped the Game row lock"
    );
    editor.commit().await.unwrap();
    assert_eq!(
        tokio::time::timeout(StdDuration::from_secs(5), admission)
            .await
            .expect("admission did not resume after Game edit")
            .unwrap()
            .unwrap(),
        AdmissionOutcome::Accepted
    );
    let (global, scoped): (i64, i64) = sqlx::query_as(
        r#"SELECT
             COUNT(*) FILTER (WHERE game_id IS NULL),
             COUNT(*) FILTER (WHERE game_id = 1)
           FROM "IdentityObservations" WHERE user_id=$1"#,
    )
    .bind(user_id)
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!((global, scoped), (1, 1));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn scoped_observations_require_live_roster_but_include_captains() {
    let harness = Harness::new().await;
    let config = test_config();
    let kicked = Uuid::new_v4();
    let captain = Uuid::new_v4();
    insert_user(&harness.pool, kicked, "kicked", "192.0.2.30").await;
    insert_user(&harness.pool, captain, "captain", "192.0.2.31").await;
    let now = Utc::now();
    sqlx::query(r#"INSERT INTO "Teams" (id,captain_id) VALUES (10,$1)"#)
        .bind(captain)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "Games" (id,start_time_utc,end_time_utc)
           VALUES (1,$1,$2)"#,
    )
    .bind(now - Duration::hours(1))
    .bind(now + Duration::hours(1))
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "Participations" (id,game_id,team_id,status)
           VALUES (1,1,10,$1)"#,
    )
    .bind(crate::utils::enums::ParticipationStatus::Accepted as i16)
    .execute(&harness.pool)
    .await
    .unwrap();
    for user_id in [kicked, captain] {
        sqlx::query(
            r#"INSERT INTO "UserParticipations"
                 (user_id,game_id,team_id,participation_id)
               VALUES ($1,1,10,1)"#,
        )
        .bind(user_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    }

    for (user_id, name, ip) in [
        (kicked, "kicked", "198.51.100.30"),
        (captain, "captain", "198.51.100.31"),
    ] {
        assert_eq!(
            admit_existing_user(
                &harness.pool,
                &config,
                user_id,
                Some(name),
                Some(ip),
                None,
                IdentitySource::Password,
                "stamp",
                None,
                crate::services::captcha::CaptchaAdmission::Local(None),
            )
            .await
            .unwrap(),
            AdmissionOutcome::Accepted
        );
    }
    let counts = sqlx::query_as::<_, (Uuid, i64, i64)>(
        r#"SELECT user_id,
                  COUNT(*) FILTER (WHERE game_id IS NULL),
                  COUNT(*) FILTER (WHERE game_id = 1)
             FROM "IdentityObservations"
            WHERE user_id IN ($1,$2)
            GROUP BY user_id
            ORDER BY user_id"#,
    )
    .bind(kicked)
    .bind(captain)
    .fetch_all(&harness.pool)
    .await
    .unwrap();
    let kicked_counts = counts.iter().find(|row| row.0 == kicked).unwrap();
    let captain_counts = counts.iter().find(|row| row.0 == captain).unwrap();
    assert_eq!((kicked_counts.1, kicked_counts.2), (1, 0));
    assert_eq!((captain_counts.1, captain_counts.2), (1, 1));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn active_game_join_records_current_identity_and_snapshots_recent_login() {
    let harness = Harness::new().await;
    let config = test_config();
    let user_id = Uuid::new_v4();
    insert_user(&harness.pool, user_id, "join-user", "192.0.2.1").await;
    let now = Utc::now();
    sqlx::query(r#"INSERT INTO "Teams" (id,captain_id) VALUES (10,$1)"#)
        .bind(user_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "Games" (id,start_time_utc,end_time_utc)
           VALUES (1,$1,$2)"#,
    )
    .bind(now - Duration::hours(1))
    .bind(now + Duration::hours(1))
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "Participations" (id,game_id,team_id,status)
           VALUES (1,1,10,$1)"#,
    )
    .bind(crate::utils::enums::ParticipationStatus::Accepted as i16)
    .execute(&harness.pool)
    .await
    .unwrap();

    // A login before the game link exists can only write its global identity.
    assert_eq!(
        admit_existing_user(
            &harness.pool,
            &config,
            user_id,
            Some("join-user"),
            Some("198.51.100.41"),
            None,
            IdentitySource::Password,
            "stamp",
            None,
            crate::services::captcha::CaptchaAdmission::Local(None),
        )
        .await
        .unwrap(),
        AdmissionOutcome::Accepted
    );

    let mut transaction = harness.pool.begin().await.unwrap();
    let mut scope = lock_game_join_identity_scope(
        &mut transaction,
        &config,
        user_id,
        Some("203.0.113.41"),
        None,
    )
    .await
    .unwrap();
    lock_game_join_observation_games(&mut transaction, user_id, 1, &mut scope)
        .await
        .unwrap();
    lock_live_request_account(&mut transaction, user_id, "stamp")
        .await
        .unwrap();
    let decision = evaluate_game_join_identity(&mut transaction, user_id, &scope)
        .await
        .unwrap();
    assert_eq!(decision.outcome(), AdmissionOutcome::Accepted);
    sqlx::query(
        r#"INSERT INTO "UserParticipations"
             (user_id,game_id,team_id,participation_id)
           VALUES ($1,1,10,1)"#,
    )
    .bind(user_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    record_game_join_identity_decision(
        &mut transaction,
        user_id,
        Some("join-user"),
        &scope,
        &decision,
    )
    .await
    .unwrap();
    assert_eq!(
        snapshot_recent_global_observations_for_game(&mut transaction, user_id, 1, 10, 1,)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        snapshot_recent_global_observations_for_game(&mut transaction, user_id, 1, 10, 1,)
            .await
            .unwrap(),
        0
    );
    transaction.commit().await.unwrap();

    let sources: Vec<String> = sqlx::query_scalar(
        r#"SELECT source FROM "IdentityObservations"
            WHERE user_id=$1 AND game_id=1 ORDER BY source"#,
    )
    .bind(user_id)
    .fetch_all(&harness.pool)
    .await
    .unwrap();
    assert_eq!(sources, vec!["GameJoin", "Password"]);
    let provenance_preserved: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1
                 FROM "IdentityObservations" global
                 JOIN "IdentityObservations" scoped
                   ON scoped.user_id=global.user_id
                  AND scoped.value_hash=global.value_hash
                  AND scoped.source=global.source
                  AND scoped.observed_at_utc=global.observed_at_utc
                WHERE global.user_id=$1 AND global.game_id IS NULL
                  AND scoped.game_id=1 AND scoped.participation_id=1
           )"#,
    )
    .bind(user_id)
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert!(provenance_preserved);
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn join_first_then_concurrent_login_cannot_lose_game_identity_context() {
    let harness = Harness::new().await;
    let config = test_config();
    let user_id = Uuid::new_v4();
    insert_user(&harness.pool, user_id, "concurrent-join", "192.0.2.1").await;
    let now = Utc::now();
    sqlx::query(r#"INSERT INTO "Teams" (id,captain_id) VALUES (10,$1)"#)
        .bind(user_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "Games" (id,start_time_utc,end_time_utc)
           VALUES (1,$1,$2)"#,
    )
    .bind(now - Duration::hours(1))
    .bind(now + Duration::hours(1))
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "Participations" (id,game_id,team_id,status)
           VALUES (1,1,10,$1)"#,
    )
    .bind(crate::utils::enums::ParticipationStatus::Accepted as i16)
    .execute(&harness.pool)
    .await
    .unwrap();

    let mut join = harness.pool.begin().await.unwrap();
    let mut scope =
        lock_game_join_identity_scope(&mut join, &config, user_id, Some("203.0.113.51"), None)
            .await
            .unwrap();
    lock_game_join_observation_games(&mut join, user_id, 1, &mut scope)
        .await
        .unwrap();
    let decision = evaluate_game_join_identity(&mut join, user_id, &scope)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "UserParticipations"
             (user_id,game_id,team_id,participation_id)
           VALUES ($1,1,10,1)"#,
    )
    .bind(user_id)
    .execute(&mut *join)
    .await
    .unwrap();
    record_game_join_identity_decision(
        &mut join,
        user_id,
        Some("concurrent-join"),
        &scope,
        &decision,
    )
    .await
    .unwrap();

    let pool = harness.pool.clone();
    let login_config = config.clone();
    let login = tokio::spawn(async move {
        admit_existing_user(
            &pool,
            &login_config,
            user_id,
            Some("concurrent-join"),
            Some("198.51.100.51"),
            None,
            IdentitySource::Password,
            "stamp",
            None,
            crate::services::captcha::CaptchaAdmission::Local(None),
        )
        .await
    });
    tokio::time::sleep(StdDuration::from_millis(25)).await;
    assert!(
        !login.is_finished(),
        "login bypassed the game-join user lock"
    );
    join.commit().await.unwrap();
    assert_eq!(
        tokio::time::timeout(StdDuration::from_secs(5), login)
            .await
            .expect("login did not resume after join commit")
            .unwrap()
            .unwrap(),
        AdmissionOutcome::Accepted
    );
    let scoped: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM "IdentityObservations"
            WHERE user_id=$1 AND game_id=1"#,
    )
    .bind(user_id)
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(scoped, 2);
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn exemption_expiring_while_identity_lock_waits_does_not_admit() {
    let harness = Harness::new().await;
    let config = test_config();
    let owner = Uuid::new_v4();
    let joining = Uuid::new_v4();
    insert_user(&harness.pool, owner, "owner", "192.0.2.80").await;
    insert_user(&harness.pool, joining, "joining", "198.51.100.80").await;
    set_policy(
        &harness.pool,
        PolicyFlags {
            require_unique_ip_global: true,
            ..Default::default()
        },
    )
    .await;
    let identity = prepare_identity(
        config.identity_hash_key.as_bytes(),
        Some("203.0.113.80"),
        None,
    );
    insert_observation(
        &harness.pool,
        owner,
        None,
        None,
        None,
        &identity.values[0],
        Utc::now(),
    )
    .await;
    let block_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "AntiCheatBlocks"
             (user_id,conflict_user_id,kind,conflicting_value,
              conflicting_value_hash,occurred_at_utc)
           VALUES ($1,$2,'Ip','203.0.113.x',$3,clock_timestamp())
           RETURNING id"#,
    )
    .bind(joining)
    .bind(owner)
    .bind(&identity.values[0].hash)
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    let (user_a, user_b) = super::exemption::canonical_pair(owner, joining);
    sqlx::query(
        r#"INSERT INTO "AntiCheatExemptions"
             (user_a,user_b,kind,value_hash,created_from_block_id,
              created_by_user_id,created_at_utc,expires_at_utc)
           VALUES ($1,$2,'Ip',$3,$4,$5,clock_timestamp(),
                   clock_timestamp() + INTERVAL '150 milliseconds')"#,
    )
    .bind(user_a)
    .bind(user_b)
    .bind(&identity.values[0].hash)
    .bind(block_id)
    .bind(Uuid::new_v4())
    .execute(&harness.pool)
    .await
    .unwrap();

    let mut lock_holder = harness.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(identity_lock_key(&identity.values[0].hash))
        .execute(&mut *lock_holder)
        .await
        .unwrap();
    let pool = harness.pool.clone();
    let admission_config = config.clone();
    let admission = tokio::spawn(async move {
        admit_existing_user(
            &pool,
            &admission_config,
            joining,
            Some("joining"),
            Some("203.0.113.80"),
            None,
            IdentitySource::Password,
            "stamp",
            None,
            crate::services::captcha::CaptchaAdmission::Local(None),
        )
        .await
    });
    tokio::time::sleep(StdDuration::from_millis(250)).await;
    lock_holder.commit().await.unwrap();
    let outcome = tokio::time::timeout(StdDuration::from_secs(5), admission)
        .await
        .expect("admission remained stalled")
        .unwrap()
        .unwrap();
    assert_eq!(outcome, AdmissionOutcome::Blocked);
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn roster_history_is_distinct_policy_filtered_and_hard_capped() {
    let harness = Harness::new().await;
    let config = test_config();
    let user_id = Uuid::new_v4();
    insert_user(&harness.pool, user_id, "history-user", "192.0.2.1").await;
    let repeated = prepare_identity(
        config.identity_hash_key.as_bytes(),
        Some("203.0.113.101"),
        None,
    );
    sqlx::query(
        r#"INSERT INTO "IdentityObservations"
             (user_id,kind,value_hash,value_hint,source,observed_at_utc)
           SELECT $1,'Ip',$2,'203.0.113.x','Password',clock_timestamp()
             FROM generate_series(1,2000)"#,
    )
    .bind(user_id)
    .bind(&repeated.values[0].hash)
    .execute(&harness.pool)
    .await
    .unwrap();
    // Fingerprint history is ignored when only the per-team IP rule is on.
    sqlx::query(
        r#"INSERT INTO "IdentityObservations"
             (user_id,kind,value_hash,value_hint,source,observed_at_utc)
           VALUES ($1,'Fingerprint',$2,'masked','Password',clock_timestamp())"#,
    )
    .bind(user_id)
    .bind(vec![9_u8; 32])
    .execute(&harness.pool)
    .await
    .unwrap();
    let policy = PolicyFlags {
        require_unique_ip_per_team_user: true,
        ..Default::default()
    };
    let mut transaction = harness.pool.begin().await.unwrap();
    let rows = tokio::time::timeout(
        StdDuration::from_secs(2),
        super::roster::recent_joining_identities(
            &mut transaction,
            policy,
            user_id,
            Utc::now() - Duration::hours(24),
        ),
    )
    .await
    .expect("distinct history query exceeded its bounded runtime")
    .unwrap();
    assert_eq!(rows.len(), 1);
    transaction.rollback().await.unwrap();

    sqlx::query(
        r#"INSERT INTO "IdentityObservations"
             (user_id,kind,value_hash,value_hint,source,observed_at_utc)
           SELECT $1,'Ip',decode(md5(series::text) || md5(series::text),'hex'),
                  'masked','Password',clock_timestamp()
             FROM generate_series(1,65) series"#,
    )
    .bind(user_id)
    .execute(&harness.pool)
    .await
    .unwrap();
    let mut transaction = harness.pool.begin().await.unwrap();
    assert!(super::roster::recent_joining_identities(
        &mut transaction,
        policy,
        user_id,
        Utc::now() - Duration::hours(24),
    )
    .await
    .is_err());
    transaction.rollback().await.unwrap();
}
