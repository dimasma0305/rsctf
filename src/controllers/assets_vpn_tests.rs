//! Event-VPN transport evidence for protected attachment downloads: the
//! tunnel-internal source versus a grant re-scoped from the live proof.

use uuid::Uuid;

use super::super::authorization::{
    finalize_grant_with_transport_for_test, finalize_monitor_grant_for_test,
    participant_grant_for_test, AssetTarget, DownloadTransport,
};
use super::{current_user, AssetAuthorizationHarness, CurrentUser, Role, TEST_SECURITY_STAMP};
use crate::services::event_security::{stamp_hash, VpnAssetGrantClaims, ASSET_GRANT_TTL_SECONDS};

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn event_vpn_gate_accepts_tunnel_source_or_live_download_grant() {
    let harness = AssetAuthorizationHarness::new().await;
    let user_id = Uuid::new_v4();
    let user = current_user(user_id);
    let captain_id = Uuid::new_v4();
    let monitor_id = Uuid::new_v4();
    let peer_id = Uuid::new_v4();
    let hash = "f".repeat(64);
    sqlx::query(
        r#"INSERT INTO "Files" (id, hash, file_size, reference_count)
           VALUES (60, $1, 512, 1)"#,
    )
    .bind(&hash)
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "AspNetUsers" (id, role) VALUES ($1, 1), ($2, 1), ($3, $4)"#)
        .bind(user_id)
        .bind(captain_id)
        .bind(monitor_id)
        .bind(Role::Monitor as i16)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Teams" (id, captain_id) VALUES (13, $1)"#)
        .bind(captain_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES (13, $1)"#)
        .bind(user_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        INSERT INTO "Attachments" (id, local_file_id) VALUES (61, 60);
        INSERT INTO "Games"
            (id, hidden, vpn_access_required, vpn_policy_revision,
             start_time_utc, end_time_utc)
        VALUES
            (209, TRUE, TRUE, 4,
             CURRENT_TIMESTAMP - interval '1 hour',
             CURRENT_TIMESTAMP + interval '1 hour');
        INSERT INTO "GameChallenges"
            (id, game_id, title, is_enabled, review_status, attachment_id)
        VALUES (1001, 209, 'Tunnel-only attachment', TRUE, 0, 61);
        INSERT INTO "Participations" (id, game_id, team_id, status)
        VALUES (21, 209, 13, 1);
        "#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "UserParticipations"
               (user_id, game_id, team_id, participation_id)
           VALUES ($1, 209, 13, 21)"#,
    )
    .bind(user_id)
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "EventVpnUserPeers"
               (id, game_id, user_id, participation_id, address, generation)
           VALUES ($1, 209, $2, 21, '10.13.42.17', 2)"#,
    )
    .bind(peer_id)
    .bind(user_id)
    .execute(&harness.pool)
    .await
    .unwrap();
    // The event is live, so the roster fence also requires the caller's
    // in-event identity observation before any download is authorized.
    sqlx::query(
        r#"INSERT INTO "IdentityObservations"
               (user_id, game_id, team_id, participation_id, observed_at_utc)
           VALUES ($1, 209, 13, 21, clock_timestamp())"#,
    )
    .bind(user_id)
    .execute(&harness.pool)
    .await
    .unwrap();

    let target = AssetTarget {
        game_id: 209,
        source_team: None,
        challenge_id: Some(1001),
    };
    let grant = participant_grant_for_test(&harness.pool, &user, &target, &hash)
        .await
        .unwrap()
        .expect("accepted hidden-game participant authorizes");
    let tunnel = "10.13.42.17".parse().unwrap();
    let public = "203.0.113.5".parse().unwrap();
    let issued_at = chrono::Utc::now().timestamp();
    let claims = |content_hash: &str, participation_id: i32, game_id: i32| VpnAssetGrantClaims {
        purpose: "asset-grant".to_string(),
        user_id,
        game_id,
        participation_id,
        content_hash: content_hash.to_string(),
        peer_id,
        peer_generation: 2,
        policy_revision: 4,
        security_stamp_hash: stamp_hash(TEST_SECURITY_STAMP),
        issued_at,
        expires_at: issued_at + ASSET_GRANT_TTL_SECONDS,
    };
    let finalize = |transport: DownloadTransport| {
        let pool = harness.pool.clone();
        let grant = grant.clone();
        async move {
            finalize_grant_with_transport_for_test(&pool, &grant, &transport, None, false).await
        }
    };
    let status = |result: Result<(), crate::utils::error::AppError>| {
        result.err().map(|error| error.status())
    };
    let unauthorized = Some(axum::http::StatusCode::UNAUTHORIZED);

    // No evidence and a public source are refused exactly as before.
    assert_eq!(
        status(finalize(DownloadTransport::default()).await),
        unauthorized
    );
    assert_eq!(
        status(
            finalize(DownloadTransport {
                source: Some(public),
                grant: None,
            })
            .await
        ),
        unauthorized
    );
    // The same-origin ingress path keeps working from the tunnel address.
    finalize(DownloadTransport {
        source: Some(tunnel),
        grant: None,
    })
    .await
    .expect("tunnel-internal source satisfies the gate");
    // A grant re-scoped from the live proof works from the public origin.
    finalize(DownloadTransport {
        source: Some(public),
        grant: Some(claims(&hash, 21, 209)),
    })
    .await
    .expect("live download grant satisfies the gate without the ingress overlay");
    finalize(DownloadTransport {
        source: None,
        grant: Some(claims(&hash, 21, 209)),
    })
    .await
    .expect("grant is sufficient without a parseable source");

    // A grant names exactly one hash, participation, event, session, peer
    // generation, and policy revision.
    let mismatches = [
        claims(&"e".repeat(64), 21, 209),
        claims(&hash, 22, 209),
        claims(&hash, 21, 210),
        VpnAssetGrantClaims {
            security_stamp_hash: stamp_hash("rotated-stamp"),
            ..claims(&hash, 21, 209)
        },
        VpnAssetGrantClaims {
            user_id: captain_id,
            ..claims(&hash, 21, 209)
        },
        VpnAssetGrantClaims {
            peer_id: Uuid::new_v4(),
            ..claims(&hash, 21, 209)
        },
        VpnAssetGrantClaims {
            peer_generation: 1,
            ..claims(&hash, 21, 209)
        },
        VpnAssetGrantClaims {
            policy_revision: 3,
            ..claims(&hash, 21, 209)
        },
    ];
    for (index, mismatch) in mismatches.into_iter().enumerate() {
        assert_eq!(
            status(
                finalize(DownloadTransport {
                    source: Some(public),
                    grant: Some(mismatch),
                })
                .await
            ),
            unauthorized,
            "mismatched grant {index} was accepted"
        );
    }

    // Revoking the peer immediately invalidates both kinds of evidence.
    sqlx::query(
        r#"UPDATE "EventVpnUserPeers" SET revoked_at_utc = clock_timestamp() WHERE id = $1"#,
    )
    .bind(peer_id)
    .execute(&harness.pool)
    .await
    .unwrap();
    assert_eq!(
        status(
            finalize(DownloadTransport {
                source: Some(tunnel),
                grant: Some(claims(&hash, 21, 209)),
            })
            .await
        ),
        unauthorized
    );
    sqlx::query(r#"UPDATE "EventVpnUserPeers" SET revoked_at_utc = NULL WHERE id = $1"#)
        .bind(peer_id)
        .execute(&harness.pool)
        .await
        .unwrap();

    // Non-membership is decided before any transport evidence matters.
    sqlx::query(r#"DELETE FROM "TeamMembers" WHERE team_id = 13 AND user_id = $1"#)
        .bind(user_id)
        .execute(&harness.pool)
        .await
        .unwrap();
    assert_eq!(
        status(
            finalize(DownloadTransport {
                source: Some(tunnel),
                grant: Some(claims(&hash, 21, 209)),
            })
            .await
        ),
        Some(axum::http::StatusCode::FORBIDDEN)
    );
    sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES (13, $1)"#)
        .bind(user_id)
        .execute(&harness.pool)
        .await
        .unwrap();

    // An organizer override and the pre-start window deactivate the gate, so
    // no evidence is needed; monitors never consult it.
    sqlx::query(
        r#"INSERT INTO "EventVpnGateOverrides" (game_id, expires_at_utc)
           VALUES (209, clock_timestamp() + interval '10 minutes')"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    finalize(DownloadTransport::default())
        .await
        .expect("active override disables the transport gate");
    sqlx::query(r#"UPDATE "EventVpnGateOverrides" SET revoked_at_utc = clock_timestamp()"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    assert_eq!(
        status(finalize(DownloadTransport::default()).await),
        unauthorized
    );
    sqlx::query(
        r#"UPDATE "Games" SET start_time_utc = clock_timestamp() + interval '1 hour'
            WHERE id = 209"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    finalize(DownloadTransport::default())
        .await
        .expect("gate is inactive before the event starts");
    let monitor = CurrentUser {
        id: monitor_id,
        role: Role::Monitor,
        name: "monitor".to_string(),
        security_stamp: TEST_SECURITY_STAMP.to_string(),
    };
    finalize_monitor_grant_for_test(&harness.pool, &monitor)
        .await
        .expect("monitor bypass is unchanged");

    harness.cleanup().await;
}
