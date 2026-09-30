//! Real-PostgreSQL identity-overlap report tests, on the shared cheat report
//! fixture.

use uuid::Uuid;

use super::{CheatReportFixture, USER_1, USER_2, USER_3, USER_4};

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn submission_addresses_show_teams_that_never_signed_in_during_the_event() {
    let fixture = CheatReportFixture::create().await;
    // Both players joined before the start: no in-window login observation,
    // only the address their flag submissions came from.
    let hash = vec![0xeeu8; 32];
    for (id, participation, team, user, at) in [
        (401, 201, 101, USER_1, "2026-06-01T12:00:00Z"),
        (402, 202, 102, USER_2, "2026-06-01T12:03:00Z"),
        (403, 201, 101, USER_1, "2026-06-01T12:09:00Z"),
    ] {
        sqlx::query(
            r#"INSERT INTO "Submissions"
                 (id, game_id, participation_id, challenge_id, team_id, user_id,
                  answer, status, submit_time_utc, submit_remote_ip_hash)
               VALUES ($1, 1, $2, 11, $3, $4, 'x', 2, $5::timestamptz, $6)"#,
        )
        .bind(id)
        .bind(participation)
        .bind(team)
        .bind(Uuid::parse_str(user).unwrap())
        .bind(at)
        .bind(&hash)
        .execute(&fixture.pool)
        .await
        .unwrap();
    }
    let (ip_rows, overlaps) =
        super::super::cheat_identity::build_identity_analysis(&fixture.pool, 1)
            .await
            .unwrap();
    assert_eq!(overlaps.len(), 1, "{overlaps:?}");
    assert_eq!(overlaps[0]["teamCount"], 2);
    assert_eq!(overlaps[0]["value"], "masked");
    assert_eq!(ip_rows.len(), 2);
    assert!(ip_rows.iter().all(|row| row["type"] == "CrossTeamIP"));
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn identity_overlap_excludes_observations_from_other_games() {
    let fixture = CheatReportFixture::create().await;
    let hash = vec![0xabu8; 32];
    for (user, game, team, participation) in [
        (USER_1, 1, 101, 201),
        (USER_3, 2, 103, 203),
        (USER_4, 2, 104, 204),
    ] {
        sqlx::query(
            r#"INSERT INTO "IdentityObservations"
                 (user_id, team_id, game_id, participation_id, kind, value_hash,
                  value_hint, observed_at_utc)
               VALUES ($1, $2, $3, $4, 'Ip', $5, '198.51.100.42',
                       '2026-06-01T12:00:00Z')"#,
        )
        .bind(Uuid::parse_str(user).unwrap())
        .bind(team)
        .bind(game)
        .bind(participation)
        .bind(&hash)
        .execute(&fixture.pool)
        .await
        .unwrap();
    }

    // A non-practice game's end is exclusive. Even two matching observations
    // at exactly `end_time_utc` must not appear in that game's report.
    let boundary_hash = vec![0xcdu8; 32];
    for (user, team, participation) in [(USER_1, 101, 201), (USER_2, 102, 202)] {
        sqlx::query(
            r#"INSERT INTO "IdentityObservations"
                 (user_id, team_id, game_id, participation_id, kind, value_hash,
                  value_hint, observed_at_utc)
               VALUES ($1, $2, 1, $3, 'Ip', $4, '203.0.113.8',
                       '2026-12-31T23:59:59Z')"#,
        )
        .bind(Uuid::parse_str(user).unwrap())
        .bind(team)
        .bind(participation)
        .bind(&boundary_hash)
        .execute(&fixture.pool)
        .await
        .unwrap();
    }

    let (ip_rows, overlaps) =
        super::super::cheat_identity::build_identity_analysis(&fixture.pool, 1)
            .await
            .unwrap();
    assert!(ip_rows.is_empty());
    assert!(overlaps.is_empty());

    // Post-end practice observations are a negative control: reports retain the
    // configured competition window rather than expanding as practice continues.
    let (practice_ip_rows, practice_overlaps) =
        super::super::cheat_identity::build_identity_analysis(&fixture.pool, 2)
            .await
            .unwrap();
    assert!(practice_ip_rows.is_empty());
    assert!(practice_overlaps.is_empty());

    sqlx::query(
        r#"INSERT INTO "IdentityObservations"
             (user_id, team_id, game_id, participation_id, kind, value_hash,
              value_hint, observed_at_utc)
           VALUES ($1, 102, 1, 202, 'Ip', $2, '198.51.100.42',
                   '2026-06-01T12:01:00Z')"#,
    )
    .bind(Uuid::parse_str(USER_2).unwrap())
    .bind(&hash)
    .execute(&fixture.pool)
    .await
    .unwrap();

    // The observation is an immutable membership snapshot. Leaving after the
    // login must not erase the historical relationship from the report.
    sqlx::query(
        r#"DELETE FROM "UserParticipations"
            WHERE user_id = $1 AND game_id = 1"#,
    )
    .bind(Uuid::parse_str(USER_2).unwrap())
    .execute(&fixture.pool)
    .await
    .unwrap();
    let (ip_rows, overlaps) =
        super::super::cheat_identity::build_identity_analysis(&fixture.pool, 1)
            .await
            .unwrap();
    assert_eq!(ip_rows.len(), 2);
    assert_eq!(overlaps.len(), 1);
    assert!(ip_rows.iter().all(|row| row["type"] == "CrossTeamIP"));
    assert_ne!(overlaps[0]["value"], "198.51.100.42");
    // The Users column and @user filter name the accounts on each side.
    assert_eq!(
        overlaps[0]["userNames"],
        serde_json::json!(["current-user-1", "current-user-2"])
    );
    let owner_row = ip_rows.iter().find(|row| row["teamId"] == 101).unwrap();
    assert_eq!(
        owner_row["userNames"],
        serde_json::json!(["current-user-1"])
    );
    assert_eq!(
        owner_row["relatedUsers"],
        serde_json::json!(["current-user-2"])
    );

    // Large exact-IP groups are suppressed as likely shared networks, but a
    // browser fingerprint shared across any number of teams remains precise
    // identity evidence and must stay visible.
    sqlx::raw_sql(
        r#"
        INSERT INTO "Teams" VALUES
          (111, 'Fingerprint 1', NULL), (112, 'Fingerprint 2', NULL),
          (113, 'Fingerprint 3', NULL), (114, 'Fingerprint 4', NULL),
          (115, 'Fingerprint 5', NULL);
        INSERT INTO "Participations" VALUES
          (211, 1, 111, 1, NULL), (212, 1, 112, 1, NULL),
          (213, 1, 113, 1, NULL), (214, 1, 114, 1, NULL),
          (215, 1, 115, 1, NULL);
        INSERT INTO "IdentityObservations"
          (user_id, team_id, game_id, participation_id, kind, value_hash,
           value_hint, observed_at_utc)
        VALUES
          ('00000000-0000-0000-0000-000000000011', 111, 1, 211, 'Fingerprint',
           decode(repeat('ef', 32), 'hex'), 'abcdef012345', '2026-06-01T12:00:00Z'),
          ('00000000-0000-0000-0000-000000000012', 112, 1, 212, 'Fingerprint',
           decode(repeat('ef', 32), 'hex'), 'abcdef012345', '2026-06-01T12:01:00Z'),
          ('00000000-0000-0000-0000-000000000013', 113, 1, 213, 'Fingerprint',
           decode(repeat('ef', 32), 'hex'), 'abcdef012345', '2026-06-01T12:02:00Z'),
          ('00000000-0000-0000-0000-000000000014', 114, 1, 214, 'Fingerprint',
           decode(repeat('ef', 32), 'hex'), 'abcdef012345', '2026-06-01T12:03:00Z'),
          ('00000000-0000-0000-0000-000000000015', 115, 1, 215, 'Fingerprint',
           decode(repeat('ef', 32), 'hex'), 'abcdef012345', '2026-06-01T12:04:00Z');
        INSERT INTO "IdentityObservations"
          (user_id, team_id, game_id, participation_id, kind, value_hash,
           value_hint, observed_at_utc)
        SELECT user_id, team_id, game_id, participation_id, 'Ip',
               decode(repeat('ad', 32), 'hex'), '203.0.113.x', observed_at_utc
          FROM "IdentityObservations"
         WHERE kind = 'Fingerprint';
        "#,
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    let (identity_rows, overlaps) =
        super::super::cheat_identity::build_identity_analysis(&fixture.pool, 1)
            .await
            .unwrap();
    assert_eq!(
        identity_rows
            .iter()
            .filter(|row| row["type"] == "SharedFingerprint")
            .count(),
        5
    );
    assert!(overlaps
        .iter()
        .any(|row| { row["kind"] == "fingerprint" && row["teamCount"] == 5 }));
    assert!(!overlaps
        .iter()
        .any(|row| { row["kind"] == "ip" && row["teamCount"] == 5 }));
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn identity_report_applies_exemptions_to_temporal_pairs_not_whole_hashes() {
    let fixture = CheatReportFixture::create().await;
    let user_a = Uuid::parse_str(USER_1).unwrap();
    let user_b = Uuid::parse_str(USER_2).unwrap();
    let user_c = Uuid::parse_str(USER_3).unwrap();
    let fingerprint_hash = vec![0x61_u8; 32];

    sqlx::raw_sql(
        r#"
        INSERT INTO "Teams" VALUES (105, 'Unexempt third', NULL);
        INSERT INTO "Participations" VALUES (205, 1, 105, 1, NULL);
        "#,
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    for (user_id, team_id, participation_id, observed_at) in [
        (user_a, 101, 201, "2026-06-01T12:01:00Z"),
        (user_b, 102, 202, "2026-06-01T12:02:00Z"),
    ] {
        sqlx::query(
            r#"INSERT INTO "IdentityObservations"
                 (user_id, team_id, game_id, participation_id, kind,
                  value_hash, value_hint, observed_at_utc)
               VALUES ($1, $2, 1, $3, 'Fingerprint', $4,
                       'abcdef012345', $5::timestamptz)"#,
        )
        .bind(user_id)
        .bind(team_id)
        .bind(participation_id)
        .bind(&fingerprint_hash)
        .bind(observed_at)
        .execute(&fixture.pool)
        .await
        .unwrap();
    }
    sqlx::query(
        r#"INSERT INTO "AntiCheatExemptions"
             (user_a, user_b, kind, value_hash, created_at_utc, expires_at_utc)
           VALUES ($1, $2, 'Fingerprint', $3,
                   '2026-06-01T12:00:00Z', '2026-06-01T13:00:00Z')"#,
    )
    .bind(user_a)
    .bind(user_b)
    .bind(&fingerprint_hash)
    .execute(&fixture.pool)
    .await
    .unwrap();

    let (ip_rows, overlaps) =
        super::super::cheat_identity::build_identity_analysis(&fixture.pool, 1)
            .await
            .unwrap();
    assert!(ip_rows.is_empty());
    assert!(overlaps.is_empty());

    sqlx::query(
        r#"INSERT INTO "IdentityObservations"
             (user_id, team_id, game_id, participation_id, kind,
              value_hash, value_hint, observed_at_utc)
           VALUES ($1, 105, 1, 205, 'Fingerprint', $2,
                   'abcdef012345', '2026-06-01T12:03:00Z')"#,
    )
    .bind(user_c)
    .bind(&fingerprint_hash)
    .execute(&fixture.pool)
    .await
    .unwrap();
    let (ip_rows, overlaps) =
        super::super::cheat_identity::build_identity_analysis(&fixture.pool, 1)
            .await
            .unwrap();
    assert_eq!(ip_rows.len(), 3, "the unexempt A-C and B-C edges remain");
    assert_eq!(overlaps.len(), 1);
    assert_eq!(overlaps[0]["teamCount"], 3);
    let team_a_row = ip_rows.iter().find(|row| row["teamId"] == 101).unwrap();
    let team_b_row = ip_rows.iter().find(|row| row["teamId"] == 102).unwrap();
    let team_c_row = ip_rows.iter().find(|row| row["teamId"] == 105).unwrap();
    assert_eq!(
        team_a_row["relatedTeams"],
        serde_json::json!(["Unexempt third"]),
        "the exempt A-B edge must not leak back through the retained group"
    );
    assert_eq!(
        team_b_row["relatedTeams"],
        serde_json::json!(["Unexempt third"])
    );
    assert_eq!(
        team_c_row["relatedTeams"],
        serde_json::json!(["Owner current", "Submit current"])
    );

    sqlx::query(r#"DELETE FROM "IdentityObservations""#)
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query(r#"DELETE FROM "AntiCheatExemptions""#)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let ip_hash = vec![0x62_u8; 32];
    for (user_id, team_id, participation_id, observed_at) in [
        (user_a, 101, 201, "2026-06-01T12:01:00Z"),
        (user_b, 102, 202, "2026-06-01T12:02:00Z"),
    ] {
        sqlx::query(
            r#"INSERT INTO "IdentityObservations"
                 (user_id, team_id, game_id, participation_id, kind,
                  value_hash, value_hint, observed_at_utc)
               VALUES ($1, $2, 1, $3, 'Ip', $4,
                       '198.51.100.x', $5::timestamptz)"#,
        )
        .bind(user_id)
        .bind(team_id)
        .bind(participation_id)
        .bind(&ip_hash)
        .bind(observed_at)
        .execute(&fixture.pool)
        .await
        .unwrap();
    }
    sqlx::query(
        r#"INSERT INTO "AntiCheatExemptions"
             (user_a, user_b, kind, value_hash, created_at_utc, expires_at_utc)
           VALUES ($1, $2, 'Ip', $3,
                   '2026-06-01T12:00:00Z', '2026-06-01T13:00:00Z')"#,
    )
    .bind(user_a)
    .bind(user_b)
    .bind(&ip_hash)
    .execute(&fixture.pool)
    .await
    .unwrap();
    let (ip_rows, overlaps) =
        super::super::cheat_identity::build_identity_analysis(&fixture.pool, 1)
            .await
            .unwrap();
    assert!(
        ip_rows.is_empty(),
        "later reconciliation cannot revive the edge"
    );
    assert!(overlaps.is_empty());

    sqlx::query(
        r#"INSERT INTO "IdentityObservations"
             (user_id, team_id, game_id, participation_id, kind,
              value_hash, value_hint, observed_at_utc)
           VALUES ($1, 102, 1, 202, 'Ip', $2,
                   '198.51.100.x', '2026-06-01T13:00:00Z')"#,
    )
    .bind(user_b)
    .bind(&ip_hash)
    .execute(&fixture.pool)
    .await
    .unwrap();
    let (ip_rows, overlaps) =
        super::super::cheat_identity::build_identity_analysis(&fixture.pool, 1)
            .await
            .unwrap();
    assert_eq!(ip_rows.len(), 2);
    assert_eq!(overlaps.len(), 1);
    assert_eq!(
        ip_rows[0]["time"],
        "2026-06-01T13:00:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap()
            .timestamp_millis()
    );

    fixture.cleanup().await;
}
