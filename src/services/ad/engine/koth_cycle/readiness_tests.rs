use super::{ready_hills, snapshot_official_config};
use crate::utils::enums::{
    ChallengeBuildStatus, ChallengeReviewStatus, ChallengeType, ParticipationStatus,
};

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn ready_hills_start_without_broken_hills_and_late_repairs_cannot_join() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&std::env::var("RSCTF_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    sqlx::raw_sql(r#"
      CREATE TEMP TABLE "Games" (id INTEGER PRIMARY KEY, koth_scoring_start_round INTEGER,
        koth_epoch_ticks INTEGER, koth_cycle_ticks INTEGER, koth_champion_cooldown_ticks INTEGER,
        koth_claim_confirmation_ticks INTEGER, practice_mode BOOLEAN);
      CREATE TEMP TABLE "GameChallenges" (id INTEGER PRIMARY KEY, game_id INTEGER,
        "Type" SMALLINT, is_enabled BOOLEAN, review_status SMALLINT, build_status SMALLINT,
        build_image_digest TEXT, ad_scoring_weight DOUBLE PRECISION);
      CREATE TEMP TABLE "KothTargets" (game_id INTEGER, challenge_id INTEGER, host TEXT, port INTEGER, container_id TEXT);
      CREATE TEMP TABLE "KothApiObservers" (game_id INTEGER, challenge_id INTEGER);
      CREATE TEMP TABLE "Participations" (id INTEGER, game_id INTEGER, status SMALLINT);
      CREATE TEMP TABLE "KothOfficialConfigs" (game_id INTEGER PRIMARY KEY, scoring_start_round INTEGER,
        epoch_ticks INTEGER, cycle_ticks INTEGER, champion_cooldown_ticks INTEGER,
        claim_confirmation_ticks INTEGER, roster_snapshot JSONB, hills_snapshot JSONB,
        excluded_hills_snapshot JSONB);
      INSERT INTO "Games" VALUES (1, 1, 12, 3, 0, 1, FALSE);
      INSERT INTO "KothTargets" VALUES (1, 10, 'hill', 80, 'container-10');
    "#).execute(&pool).await.unwrap();
    sqlx::query(r#"INSERT INTO "GameChallenges" SELECT id, 1, $1, TRUE, $2, $3, 'sha256:fixture', 1.0 FROM unnest(ARRAY[10,11,12]) id"#)
        .bind(ChallengeType::KingOfTheHill as i16).bind(ChallengeReviewStatus::Active as i16)
        .bind(ChallengeBuildStatus::Success as i16).execute(&pool).await.unwrap();
    sqlx::query(r#"INSERT INTO "Participations" VALUES (1, 1, $1), (2, 1, $1)"#)
        .bind(ParticipationStatus::Accepted as i16)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(ready_hills(&mut tx, 2, &[10, 11, 12])
        .await
        .unwrap()
        .ids
        .is_empty());
    let none = ready_hills(&mut tx, 1, &[]).await.unwrap();
    assert!(none.ids.is_empty());
    assert!(snapshot_official_config(&mut tx, 1, 1, &none)
        .await
        .is_err());
    let ready = ready_hills(&mut tx, 1, &[10, 11]).await.unwrap();
    assert_eq!(ready.ids, vec![10]);
    let targets_locked: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(SELECT 1 FROM pg_locks WHERE pid = pg_backend_pid()
           AND relation = 'pg_temp."KothTargets"'::regclass
           AND mode = 'RowShareLock' AND granted)"#,
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert!(
        targets_locked,
        "target teardown must wait until the scoring snapshot commits"
    );
    assert_eq!(
        ready.excluded[0],
        serde_json::json!({"challengeId":11,"reasons":["TargetNotReady"]})
    );
    assert_eq!(
        ready.excluded[1],
        serde_json::json!({"challengeId":12,"reasons":["TargetNotReady","CheckerNotReady"]})
    );
    snapshot_official_config(&mut tx, 1, 1, &ready)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "KothTargets" VALUES (1,11,'hill',80,'container-11'), (1,12,'hill',80,'container-12')"#)
        .execute(&mut *tx).await.unwrap();
    let repaired = ready_hills(&mut tx, 1, &[10, 11, 12]).await.unwrap();
    assert_eq!(repaired.ids, vec![10, 11, 12]);
    snapshot_official_config(&mut tx, 1, 1, &repaired)
        .await
        .unwrap();
    let (hills, excluded, roster): (serde_json::Value, serde_json::Value, serde_json::Value) =
        sqlx::query_as(r#"SELECT hills_snapshot, excluded_hills_snapshot, roster_snapshot FROM "KothOfficialConfigs" WHERE game_id = 1"#)
        .fetch_one(&mut *tx).await.unwrap();
    assert_eq!(hills.as_array().unwrap().len(), 1);
    assert_eq!(hills[0]["challengeId"], 10);
    assert_eq!(excluded, serde_json::json!(ready.excluded));
    assert_eq!(roster, serde_json::json!([1, 2]));
    tx.rollback().await.unwrap();
    pool.close().await;
}
