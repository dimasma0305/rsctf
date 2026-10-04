//! Real PostgreSQL checks for unlisted event links and normal registration.

use axum::extract::ConnectInfo;
use axum::http::Request;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::ai_chats::tests::{body, fixture, Fixture};
use super::*;

async fn request(f: &Fixture, path: &str, viewer: Option<&CurrentUser>) -> Response {
    let mut request = Request::builder().uri(path);
    if let Some(viewer) = viewer {
        let token =
            f.st.token
                .issue(viewer.id, viewer.role, &viewer.name, &viewer.security_stamp)
                .unwrap();
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    router()
        .merge(crate::controllers::edit::router())
        .with_state(f.st.clone())
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn details(f: &Fixture, viewer: Option<&CurrentUser>) -> Value {
    let response = request(f, &format!("/api/game/{}", f.open_game), viewer).await;
    assert_eq!(response.status(), StatusCode::OK);
    body(response).await
}

async fn configure(f: &Fixture, sql: &str) {
    sqlx::query(sql)
        .bind(f.open_game)
        .execute(&f.pool)
        .await
        .unwrap();
    scoreboard_board::invalidate_game_row_cache(f.open_game);
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn hidden_links_allow_normal_reads_without_challenge_or_operator_privileges() {
    let f = fixture().await;
    configure(&f, r#"UPDATE "Games" SET hidden = TRUE WHERE id = $1"#).await;

    for viewer in [None, Some(&f.outsider), Some(&f.pending)] {
        let info = details(&f, viewer).await;
        assert_eq!(info["hidden"], true);
        assert!(info.get("challenges").is_none());
        assert!(info.get("privateKey").is_none());
        assert!(info.get("inviteCode").is_none());
        for suffix in [
            "scoreboard",
            "scoreboard/combined",
            "ad/scoreboard",
            "ad/koth/scoreboard",
            "ad/koth/timeline",
            "notices",
        ] {
            let response =
                request(&f, &format!("/api/game/{}/{suffix}", f.open_game), viewer).await;
            assert_eq!(response.status(), StatusCode::OK, "{suffix}");
        }
    }
    let accepted = details(&f, Some(&f.alice)).await;
    assert!(accepted["challenges"].is_object());
    let catalog = body(request(&f, "/api/game?count=100", Some(&f.alice)).await).await;
    assert!(catalog["data"]
        .as_array()
        .unwrap()
        .iter()
        .all(|game| game["id"] != f.open_game));

    let params = HashMap::from([("game".to_owned(), f.open_game.to_string())]);
    let scope = crate::hubs::signalr::public_game_scope(&f.st, &params, &HeaderMap::new())
        .await
        .expect("anonymous live updates for an unlisted event");
    assert_eq!(scope.game_id, f.open_game);
    assert!(scope.authorization.as_ref().unwrap().is_valid().await);
    assert!(matches!(
        crate::hubs::signalr::public_game_scope(&f.st, &HashMap::new(), &HeaderMap::new()).await,
        Err(StatusCode::BAD_REQUEST)
    ));
    assert_eq!(
        request(
            &f,
            &format!("/api/edit/games/{}", f.open_game),
            Some(&f.outsider)
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );

    let challenge_path = format!("/api/game/{}/challenges/{}", f.open_game, f.solved);
    for viewer in [None, Some(&f.outsider), Some(&f.pending)] {
        assert_ne!(
            request(&f, &challenge_path, viewer).await.status(),
            StatusCode::OK
        );
    }
    assert_eq!(
        request(&f, &challenge_path, Some(&f.alice)).await.status(),
        StatusCode::OK
    );
    for status in [
        ParticipationStatus::Rejected,
        ParticipationStatus::Suspended,
    ] {
        sqlx::query(r#"UPDATE "Participations" SET status = $2 WHERE game_id = $1 AND team_id IN (SELECT team_id FROM "TeamMembers" WHERE user_id = $3)"#)
            .bind(f.open_game).bind(status as i16).bind(f.pending.id).execute(&f.pool).await.unwrap();
        assert!(details(&f, Some(&f.pending))
            .await
            .get("challenges")
            .is_none());
        assert_ne!(
            request(&f, &challenge_path, Some(&f.pending))
                .await
                .status(),
            StatusCode::OK
        );
    }
    sqlx::query(r#"UPDATE "GameChallenges" SET is_enabled = FALSE WHERE id = $1"#)
        .bind(f.solved)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(
        request(&f, &challenge_path, Some(&f.alice)).await.status(),
        StatusCode::NOT_FOUND
    );

    // Hidden is not a substitute for disabled challenges or participation review.
    configure(
        &f,
        r#"UPDATE "Games" SET start_time_utc = now() + interval '1 hour', end_time_utc = now() + interval '2 hours', writeup_deadline = now() + interval '3 hours' WHERE id = $1"#,
    )
    .await;
    assert!(details(&f, Some(&f.alice))
        .await
        .get("challenges")
        .is_none());
    for suffix in [
        "scoreboard",
        "scoreboard/combined",
        "ad/scoreboard",
        "ad/koth/scoreboard",
        "ad/koth/timeline",
        "notices",
    ] {
        assert_ne!(
            request(&f, &format!("/api/game/{}/{suffix}", f.open_game), None)
                .await
                .status(),
            StatusCode::OK
        );
    }
    configure(
        &f,
        r#"UPDATE "Games" SET deletion_pending = TRUE WHERE id = $1"#,
    )
    .await;
    assert_eq!(
        request(&f, &format!("/api/game/{}", f.open_game), None)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert!(!scope.authorization.as_ref().unwrap().is_valid().await);
    f.teardown().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn hidden_registration_preserves_invites_membership_and_approval() {
    let f = fixture().await;
    configure(&f, r#"UPDATE "Games" SET hidden = TRUE, end_time_utc = now() + interval '1 day', invite_code = 'event-invite', accept_without_review = FALSE WHERE id = $1"#).await;
    let team_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "Teams" (name, locked, invite_token, captain_id)
           VALUES ('Direct link team', FALSE, 'team-invite', $1) RETURNING id"#,
    )
    .bind(f.outsider.id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES ($1, $2)"#)
        .bind(team_id)
        .bind(f.outsider.id)
        .execute(&f.pool)
        .await
        .unwrap();

    let join = |invite: Option<&str>, viewer: CurrentUser| {
        play::join_game(
            State(f.st.clone()),
            ConnectInfo("127.0.0.1:12345".parse().unwrap()),
            HeaderMap::new(),
            viewer,
            Path(f.open_game),
            axum::Json(GameJoinModel {
                team_id,
                division_id: None,
                invite_code: invite.map(str::to_owned),
                fingerprint: None,
                fingerprint_proof: None,
            }),
        )
    };
    assert!(matches!(
        join(None, f.outsider.clone()).await,
        Err(AppError::BadRequest(_))
    ));
    assert!(matches!(
        join(Some("wrong"), f.outsider.clone()).await,
        Err(AppError::BadRequest(_))
    ));
    assert!(join(Some("event-invite"), f.bob.clone()).await.is_err());
    assert_eq!(
        join(Some("event-invite"), f.outsider.clone())
            .await
            .unwrap(),
        StatusCode::OK
    );
    let joined = details(&f, Some(&f.outsider)).await;
    assert_eq!(joined["status"], "Pending");
    assert!(joined.get("challenges").is_none());

    let anonymous = router()
        .with_state(f.st.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/game/{}", f.open_game))
                .extension(ConnectInfo(
                    "127.0.0.1:12345".parse::<std::net::SocketAddr>().unwrap(),
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"teamId": team_id, "inviteCode": "event-invite"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    // A second unlisted event with open admission uses normal automatic acceptance.
    sqlx::query(r#"UPDATE "Games" SET hidden = TRUE, end_time_utc = now() + interval '1 day' WHERE id = $1"#)
        .bind(f.off_game).execute(&f.pool).await.unwrap();
    play::join_game(
        State(f.st.clone()),
        ConnectInfo("127.0.0.1:12345".parse().unwrap()),
        HeaderMap::new(),
        f.outsider.clone(),
        Path(f.off_game),
        axum::Json(GameJoinModel {
            team_id,
            division_id: None,
            invite_code: None,
            fingerprint: None,
            fingerprint_proof: None,
        }),
    )
    .await
    .unwrap();
    let accepted =
        body(request(&f, &format!("/api/game/{}", f.off_game), Some(&f.outsider)).await).await;
    assert_eq!(accepted["status"], "Accepted");
    assert!(accepted["challenges"].is_object());
    f.teardown().await;
}
