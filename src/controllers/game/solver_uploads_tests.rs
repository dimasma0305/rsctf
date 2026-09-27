//! Real-PostgreSQL boundaries for optional solver uploads through the actual
//! handlers: event switch, solve gate, team scope, versions, quota, operation
//! replay, roster fencing, and the inert monitor download.

use axum::extract::FromRequest;
use axum::response::IntoResponse;
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use super::super::ai_chats::tests::{body, fixture, status, Fixture};
use super::*;
use crate::middlewares::privilege_authentication::MonitorUser;

const SOLVER: &[u8] = b"from pwn import *\nprint('flag')\n";

async fn multipart(name: &str, content: &[u8]) -> Multipart {
    let mut payload = format!(
        "--XBOUNDARY\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\
         Content-Type: text/x-python\r\n\r\n"
    )
    .into_bytes();
    payload.extend_from_slice(content);
    payload.extend_from_slice(b"\r\n--XBOUNDARY--\r\n");
    let request = axum::http::Request::builder()
        .header(
            header::CONTENT_TYPE,
            "multipart/form-data; boundary=XBOUNDARY",
        )
        .body(axum::body::Body::from(payload))
        .unwrap();
    Multipart::from_request(request, &()).await.unwrap()
}

fn peer() -> axum::extract::ConnectInfo<std::net::SocketAddr> {
    axum::extract::ConnectInfo("203.0.113.7:4242".parse().unwrap())
}

impl Fixture {
    async fn enable_solver_uploads(&self) {
        sqlx::query(r#"UPDATE "Games" SET solver_uploads_enabled = ai_chat_links_enabled"#)
            .execute(&self.pool)
            .await
            .unwrap();
        for game in [self.open_game, self.off_game, self.closed_game] {
            super::super::scoreboard_board::invalidate_game_row_cache(game);
        }
    }

    async fn solver_state(
        &self,
        user: &CurrentUser,
        game: i32,
        challenge: i32,
    ) -> AppResult<Response> {
        get_solver_uploads(
            State(self.st.clone()),
            user.clone(),
            Path((game, challenge)),
        )
        .await
    }

    async fn upload(
        &self,
        user: &CurrentUser,
        game: i32,
        challenge: i32,
        operation: Uuid,
        name: &str,
        content: &[u8],
    ) -> AppResult<Response> {
        let mut headers = HeaderMap::new();
        headers.insert(
            crate::utils::upload::OPERATION_ID_HEADER,
            operation.to_string().parse().unwrap(),
        );
        submit_solver_upload(
            State(self.st.clone()),
            user.clone(),
            headers,
            Path((game, challenge)),
            peer(),
            multipart(name, content).await,
        )
        .await
    }

    async fn participation(&self, user: &CurrentUser) -> (i32, i32) {
        sqlx::query_as(
            r#"SELECT participation_id, team_id FROM "UserParticipations"
                WHERE user_id = $1 AND game_id = $2"#,
        )
        .bind(user.id)
        .bind(self.open_game)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn store(
        &self,
        user: &CurrentUser,
        challenge: i32,
        stamp: &str,
        content: &[u8],
    ) -> AppResult<i32> {
        let (participation_id, team_id) = self.participation(user).await;
        store_solver_upload(
            &self.pool,
            NewSolverUpload {
                game_id: self.open_game,
                team_id,
                participation_id,
                challenge_id: challenge,
                user_id: user.id,
                security_stamp: stamp,
                operation_id: Uuid::new_v4(),
                file_name: "solve.py".into(),
                content,
                remote_ip_hash: None,
            },
        )
        .await
    }
}

fn error_status<T>(result: AppResult<T>) -> u16 {
    match result {
        Ok(_) => 200,
        Err(error) => error.into_response().status().as_u16(),
    }
}

#[test]
fn file_names_are_display_safe_basenames() {
    assert_eq!(clean_file_name(Some("../../etc/passwd")), "passwd");
    assert_eq!(clean_file_name(Some("C:\\tmp\\solve.py")), "solve.py");
    assert_eq!(clean_file_name(Some("a\u{0}b\r\n.py")), "ab.py");
    assert_eq!(clean_file_name(Some("..")), "solver");
    assert_eq!(clean_file_name(None), "solver");
    assert_eq!(clean_file_name(Some(&"x".repeat(300))).chars().count(), 128);
}

#[tokio::test]
#[ignore = "requires RSCTF_TEST_DATABASE_URL"]
async fn solver_uploads_are_switched_solve_gated_team_scoped_and_bounded() {
    let f = fixture().await;
    // Off by default: every game starts without solver uploads.
    let enabled: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "Games" WHERE solver_uploads_enabled"#)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(enabled, 0);
    assert_eq!(
        status(f.solver_state(&f.alice, f.open_game, f.solved).await),
        404
    );
    f.enable_solver_uploads().await;

    // Negative boundaries.
    assert_eq!(
        status(f.solver_state(&f.alice, f.off_game, f.off_solved).await),
        404
    );
    assert_ne!(
        status(f.solver_state(&f.outsider, f.open_game, f.solved).await),
        200
    );
    assert_ne!(
        status(f.solver_state(&f.pending, f.open_game, f.solved).await),
        200
    );
    assert_eq!(
        status(
            f.solver_state(&f.alice, f.open_game, f.attack_defense)
                .await
        ),
        404
    );
    let unsolved = body(
        f.solver_state(&f.alice, f.open_game, f.unsolved)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(unsolved["solved"], false);
    assert_eq!(unsolved["editable"], false);
    let op = Uuid::new_v4();
    assert_eq!(
        status(
            f.upload(&f.alice, f.open_game, f.unsolved, op, "a.py", SOLVER)
                .await
        ),
        400
    );
    assert_eq!(
        status(
            f.upload(&f.alice, f.closed_game, f.closed_solved, op, "a.py", SOLVER)
                .await
        ),
        400,
        "the review window closed"
    );
    assert_eq!(
        status(
            f.upload(&f.outsider, f.open_game, f.solved, op, "a.py", SOLVER)
                .await
        ),
        400
    );
    assert_eq!(
        status(
            f.upload(&f.alice, f.open_game, f.solved, op, "empty.py", b"")
                .await
        ),
        400
    );
    let too_large = vec![b'a'; MAX_FILE_BYTES + 1];
    assert_eq!(
        status(
            f.upload(&f.alice, f.open_game, f.solved, op, "big.bin", &too_large)
                .await
        ),
        400
    );

    // A real upload records server timing, the hash, and a network hash; a
    // retried operation id does not create a second version.
    let first: JsonValue = body(
        f.upload(&f.alice, f.open_game, f.solved, op, "../solve.py", SOLVER)
            .await
            .unwrap(),
    )
    .await;
    let replay: JsonValue = body(
        f.upload(&f.alice, f.open_game, f.solved, op, "solve.py", SOLVER)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(first["versions"], replay["versions"]);
    let versions = first["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0]["version"], 1);
    assert_eq!(versions[0]["fileName"], "solve.py");
    assert_eq!(versions[0]["uploadedBy"], "alice");
    assert_eq!(versions[0]["sha256"], hex::encode(Sha256::digest(SOLVER)));
    let delay = versions[0]["secondsSinceSolve"].as_i64().unwrap();
    assert!((5_390..5_500).contains(&delay), "delay {delay}");
    assert_eq!(first["teamBytesUsed"], SOLVER.len());
    let ip_hashed: bool = sqlx::query_scalar(
        r#"SELECT remote_ip_hash IS NOT NULL AND solved_at IS NOT NULL FROM "SolverUploads""#,
    )
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert!(ip_hashed);
    assert_eq!(
        status(
            f.upload(&f.alice, f.open_game, f.unsolved, op, "solve.py", SOLVER)
                .await
        ),
        400,
        "still unsolved even with a reused operation id"
    );

    // Another team never sees Alpha's versions.
    let beta = body(f.solver_state(&f.bob, f.open_game, f.solved).await.unwrap()).await;
    assert_eq!(beta["versions"].as_array().unwrap().len(), 0);
    assert_eq!(beta["teamBytesUsed"], 0);

    // A stale security stamp (kicked or signed out) is fenced out.
    assert_eq!(
        error_status(f.store(&f.alice, f.solved, "stale", SOLVER).await),
        403
    );

    // Versions are capped per challenge.
    for expected in 2..=MAX_VERSIONS {
        assert_eq!(
            f.store(&f.alice, f.solved, "stamp", SOLVER).await.unwrap(),
            expected
        );
    }
    assert_eq!(
        error_status(f.store(&f.alice, f.solved, "stamp", SOLVER).await),
        400
    );
    let capped = body(
        f.solver_state(&f.alice, f.open_game, f.solved)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(capped["editable"], false);
    assert_eq!(capped["versions"][0]["version"], MAX_VERSIONS);

    // The team byte quota spans every challenge of the event.
    let (participation, team) = f.participation(&f.bob).await;
    sqlx::query(
        r#"WITH submission AS (
               INSERT INTO "Submissions"
                 (answer, status, submit_time_utc, user_id, team_id, participation_id,
                  game_id, challenge_id)
               VALUES ('flag{y}', 1, now(), $1, $2, $3, $4, $5)
               RETURNING id
           )
           INSERT INTO "FirstSolves" (participation_id, challenge_id, submission_id)
           SELECT $3, $5, id FROM submission"#,
    )
    .bind(f.bob.id)
    .bind(team)
    .bind(participation)
    .bind(f.open_game)
    .bind(f.unsolved)
    .execute(&f.pool)
    .await
    .unwrap();
    let chunk = vec![b'z'; MAX_FILE_BYTES];
    for _ in 0..MAX_VERSIONS {
        f.store(&f.bob, f.solved, "stamp", &chunk).await.unwrap();
    }
    let remaining = (MAX_TEAM_BYTES / MAX_FILE_BYTES as i64) - i64::from(MAX_VERSIONS);
    for _ in 0..remaining {
        f.store(&f.bob, f.unsolved, "stamp", &chunk).await.unwrap();
    }
    assert_eq!(
        error_status(f.store(&f.bob, f.unsolved, "stamp", b"x").await),
        400,
        "the team quota is full"
    );
    let bob_state = body(
        f.solver_state(&f.bob, f.open_game, f.unsolved)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(bob_state["teamBytesUsed"], MAX_TEAM_BYTES);

    f.teardown().await;
}

#[tokio::test]
#[ignore = "requires RSCTF_TEST_DATABASE_URL"]
async fn monitors_review_every_version_and_download_it_inert() {
    let f = fixture().await;
    f.enable_solver_uploads().await;
    for content in [SOLVER, b"second".as_slice()] {
        f.upload(
            &f.alice,
            f.open_game,
            f.solved,
            Uuid::new_v4(),
            "<script>.html",
            content,
        )
        .await
        .unwrap();
    }
    f.upload(
        &f.bob,
        f.open_game,
        f.solved,
        Uuid::new_v4(),
        "b.py",
        SOLVER,
    )
    .await
    .unwrap();
    // Disabling the switch hides the player surface but keeps review data.
    sqlx::query(r#"UPDATE "Games" SET solver_uploads_enabled = FALSE"#)
        .execute(&f.pool)
        .await
        .unwrap();
    super::super::scoreboard_board::invalidate_game_row_cache(f.open_game);

    let page: JsonValue = body(
        list_solver_uploads(
            State(f.st.clone()),
            MonitorUser(f.admin.clone()),
            Path(f.open_game),
            axum::extract::Query(SolverUploadMonitorQuery {
                count: Some(50),
                skip: Some(0),
                challenge_id: Some(f.solved),
            }),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(page["total"], 2);
    let alpha = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["teamName"] == "Alpha")
        .unwrap();
    let versions = alpha["versions"].as_array().unwrap();
    assert_eq!(
        versions
            .iter()
            .map(|version| version["version"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        [2, 1]
    );
    assert!(versions[0].get("content").is_none(), "metadata only");

    let upload_id = versions[1]["id"].as_i64().unwrap();
    let response = download_solver_upload(
        State(f.st.clone()),
        MonitorUser(f.admin.clone()),
        Path((f.open_game, upload_id)),
    )
    .await
    .unwrap();
    let headers = response.headers().clone();
    assert_eq!(headers[header::CONTENT_TYPE], "application/octet-stream");
    assert!(headers[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap()
        .starts_with("attachment;"));
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(headers[header::CONTENT_SECURITY_POLICY], "sandbox");
    assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(bytes.as_ref(), SOLVER);

    // A download id from another game is not found.
    assert_eq!(
        error_status(
            download_solver_upload(
                State(f.st.clone()),
                MonitorUser(f.admin.clone()),
                Path((f.off_game, upload_id)),
            )
            .await
        ),
        404
    );
    f.teardown().await;
}
