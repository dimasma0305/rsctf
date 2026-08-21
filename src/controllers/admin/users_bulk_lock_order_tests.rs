use std::time::Duration;

use super::tests::{explicit_write, insert_user, Harness};
use super::*;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn bulk_assignment_takes_game_fence_before_account_row_lock() {
    let harness = Harness::new().await;
    let joining_user = Uuid::new_v4();
    let bulk_captain = Uuid::new_v4();
    let public_captain = Uuid::new_v4();
    for (id, name) in [
        (joining_user, "joining"),
        (bulk_captain, "bulk-captain"),
        (public_captain, "public-captain"),
    ] {
        insert_user(
            &harness.pool,
            id,
            name,
            &format!("{name}@example.test"),
            Role::User,
            "old-hash",
            "old-stamp",
        )
        .await;
    }
    sqlx::query(
        r#"INSERT INTO "Teams"
             (id, name, locked, deletion_pending, invite_token, captain_id)
           VALUES (10, 'bulk-target', FALSE, FALSE, 'bulk-token', $1),
                  (20, 'public-target', FALSE, FALSE, 'public-token', $2)"#,
    )
    .bind(bulk_captain)
    .bind(public_captain)
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::raw_sql(
        r#"INSERT INTO "Games"
             (id, end_time_utc, ad_scoring_start_round, koth_scoring_start_round)
           VALUES (30, clock_timestamp() + interval '1 hour', NULL, NULL);
           INSERT INTO "Participations" (game_id, team_id, status)
           VALUES (30, 10, 1), (30, 20, 1)"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();

    // Model public acceptance for a different team in the same game: it owns
    // that roster and the shared game fence, then needs the joining account.
    let mut public_accept = harness.pool.begin().await.unwrap();
    crate::utils::single_flight::acquire_transaction_advisory_lock(
        &mut public_accept,
        "team-roster:20",
    )
    .await
    .unwrap();
    crate::controllers::team::ensure_roster_change_allowed(&mut public_accept, 20)
        .await
        .unwrap();

    let bulk = tokio::spawn({
        let pool = harness.pool.clone();
        async move {
            provision_explicit_user(
                &pool,
                explicit_write("JOINING", "JOINING@EXAMPLE.TEST", "bulk-password-hash"),
                Some("bulk-target"),
                Some(10),
            )
            .await
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !bulk.is_finished(),
        "bulk writer skipped the held game fence"
    );

    // The fixed bulk path is waiting on game 30 without owning this row. The
    // former account-before-game order blocked here and formed a two-way cycle.
    tokio::time::timeout(
        Duration::from_secs(2),
        sqlx::query(r#"SELECT id FROM "AspNetUsers" WHERE id = $1 FOR UPDATE"#)
            .bind(joining_user)
            .execute(&mut *public_accept),
    )
    .await
    .expect("bulk/account lock order deadlocked")
    .unwrap();
    public_accept.commit().await.unwrap();

    let provisioned = tokio::time::timeout(Duration::from_secs(5), bulk)
        .await
        .expect("bulk writer did not drain after game fence release")
        .unwrap()
        .unwrap();
    assert_eq!(provisioned.id, joining_user);
    assert_eq!(provisioned.team_id, Some(10));
    harness.cleanup().await;
}
