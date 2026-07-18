use super::delete_ad_game_data;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn game_cleanup_is_complete_scoped_and_idempotent() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TEMP TABLE "Participations" (id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL);
        CREATE TEMP TABLE "AdRounds" (id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL);
        CREATE TEMP TABLE "AdTeamServices" (id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL);
        CREATE TEMP TABLE "AdFlagDeliveryResults" (round_id INTEGER, team_service_id INTEGER);
        CREATE TEMP TABLE "AdAttacks" (
          id INTEGER PRIMARY KEY, round_id INTEGER, attacker_participation_id INTEGER,
          victim_team_service_id INTEGER, flag_id INTEGER
        );
        CREATE TEMP TABLE "AdCheckResults" (id INTEGER PRIMARY KEY, round_id INTEGER, team_service_id INTEGER);
        CREATE TEMP TABLE "AdFlags" (id INTEGER PRIMARY KEY, round_id INTEGER, team_service_id INTEGER);
        CREATE TEMP TABLE "AdEpochRollups" (game_id INTEGER, epoch INTEGER);
        CREATE TEMP TABLE "AdEpochServiceRollups" (game_id INTEGER, epoch INTEGER);
        CREATE TEMP TABLE "AdEpochTeamRollups" (game_id INTEGER, epoch INTEGER);
        CREATE TEMP TABLE "AdTeamApiTokens" (id INTEGER PRIMARY KEY, participation_id INTEGER);
        CREATE TEMP TABLE "AdSshKeys" (id INTEGER PRIMARY KEY, participation_id INTEGER);
        CREATE TEMP TABLE "AdVpnPeers" (id INTEGER PRIMARY KEY, game_id INTEGER);
        CREATE TEMP TABLE "KothAcquisitions" (id INTEGER PRIMARY KEY, game_id INTEGER);
        CREATE TEMP TABLE "KothControlResults" (id INTEGER PRIMARY KEY, game_id INTEGER);
        CREATE TEMP TABLE "KothTokens" (
          id INTEGER PRIMARY KEY, ad_round_id INTEGER, participation_id INTEGER
        );

        INSERT INTO "Participations" VALUES (11, 1), (22, 2);
        INSERT INTO "AdRounds" VALUES (101, 1), (202, 2);
        INSERT INTO "AdTeamServices" VALUES (111, 1), (222, 2);
        INSERT INTO "AdFlags" VALUES (1001, 101, 111), (2002, 202, 222);
        INSERT INTO "AdFlagDeliveryResults" VALUES (101, 111), (202, 222);
        INSERT INTO "AdCheckResults" VALUES (10001, 101, 111), (20002, 202, 222);
        INSERT INTO "AdAttacks" VALUES (1, 101, 11, 111, 1001), (2, 202, 22, 222, 2002);
        INSERT INTO "AdEpochRollups" VALUES (1, 1), (2, 1);
        INSERT INTO "AdEpochServiceRollups" VALUES (1, 1), (2, 1);
        INSERT INTO "AdEpochTeamRollups" VALUES (1, 1), (2, 1);
        INSERT INTO "AdTeamApiTokens" VALUES (1, 11), (2, 22);
        INSERT INTO "AdSshKeys" VALUES (1, 11), (2, 22);
        INSERT INTO "AdVpnPeers" VALUES (1, 1), (2, 2);
        INSERT INTO "KothAcquisitions" VALUES (1, 1), (2, 2);
        INSERT INTO "KothControlResults" VALUES (1, 1), (2, 2);
        INSERT INTO "KothTokens" VALUES (1, 101, 11), (2, 202, 22);
        "#,
    )
    .execute(&mut *tx)
    .await
    .unwrap();

    delete_ad_game_data(&mut tx, 1).await.unwrap();
    delete_ad_game_data(&mut tx, 1).await.unwrap();

    for table in [
        "AdFlagDeliveryResults",
        "AdAttacks",
        "AdCheckResults",
        "AdFlags",
        "AdEpochServiceRollups",
        "AdEpochTeamRollups",
        "AdEpochRollups",
        "AdTeamApiTokens",
        "AdSshKeys",
        "AdVpnPeers",
        "KothAcquisitions",
        "KothControlResults",
        "KothTokens",
        "AdTeamServices",
        "AdRounds",
    ] {
        let count: i64 = sqlx::query_scalar(&format!(r#"SELECT COUNT(*) FROM "{table}""#))
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(count, 1, "{table} should retain only game 2 data");
    }
    tx.rollback().await.unwrap();
}
