//! Real-PostgreSQL agent-artifact detection through the solver, writeup, and
//! AI disclosure paths: evidence rows, suspicion events, the contradiction in
//! both orders, idempotent rescans, and the monitor report and evidence view.

use axum::response::IntoResponse;
use serde_json::Value as JsonValue;

use super::super::ai_chats::tests::{fixture, Fixture};
use super::*;
use crate::middlewares::privilege_authentication::MonitorUser;
use crate::services::agent_artifacts;

const AGENT_SOLVER: &[u8] = b"from eth_account import Account\n\
# /tmp/claude-0/-home-player-ctf/0b1c2d3e-4f50-6172-8394-a5b6c7d8e9f0/scratchpad/grind_key.txt\n\
print('grind')\n";
const CLEAN_SOLVER: &[u8] = b"from pwn import *\nprint('clean')\n";

impl Fixture {
    async fn enable_uploads(&self) {
        sqlx::query(r#"UPDATE "Games" SET solver_uploads_enabled = ai_chat_links_enabled"#)
            .execute(&self.pool)
            .await
            .unwrap();
        // Real teams were admitted while the event ran. The fixture inserts them
        // after its window, so reopen the event, re-accept them (the database
        // stamps the admission), and close it again.
        sqlx::query(r#"UPDATE "Games" SET end_time_utc = now() + INTERVAL '1 hour' WHERE id = $1"#)
            .bind(self.open_game)
            .execute(&self.pool)
            .await
            .unwrap();
        let accepted: Vec<i32> = sqlx::query_scalar(
            r#"UPDATE "Participations" SET status = 0
                WHERE game_id = $1 AND status = 1 RETURNING id"#,
        )
        .bind(self.open_game)
        .fetch_all(&self.pool)
        .await
        .unwrap();
        sqlx::query(r#"UPDATE "Participations" SET status = 1 WHERE id = ANY($1)"#)
            .bind(&accepted)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query(
            r#"UPDATE "Games" SET end_time_utc = now() - INTERVAL '1 minute' WHERE id = $1"#,
        )
        .bind(self.open_game)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(r#"UPDATE "Games" SET writeup_required = TRUE WHERE id = $1"#)
            .bind(self.open_game)
            .execute(&self.pool)
            .await
            .unwrap();
        for game in [self.open_game, self.off_game, self.closed_game] {
            super::super::scoreboard_board::invalidate_game_row_cache(game);
        }
    }

    async fn membership(&self, user: &CurrentUser) -> (i32, i32) {
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

    async fn upload_solver(&self, user: &CurrentUser, content: &[u8]) -> i64 {
        self.upload_solver_for(user, self.solved, content).await
    }

    async fn upload_solver_for(
        &self,
        user: &CurrentUser,
        challenge_id: i32,
        content: &[u8],
    ) -> i64 {
        let (participation_id, team_id) = self.membership(user).await;
        store_solver_upload(
            &self.pool,
            NewSolverUpload {
                game_id: self.open_game,
                team_id,
                participation_id,
                challenge_id,
                user_id: user.id,
                security_stamp: "stamp",
                operation_id: Uuid::new_v4(),
                file_name: "solve.py".into(),
                content,
                remote_ip_hash: None,
            },
        )
        .await
        .unwrap()
        .0
    }

    async fn declare_no_ai(&self, user: &CurrentUser, revision: i32) {
        self.save_disclosure(user, &[], true, revision).await;
    }

    async fn save_disclosure(
        &self,
        user: &CurrentUser,
        links: &[&str],
        no_ai: bool,
        revision: i32,
    ) {
        let response = super::super::ai_chats::save_ai_chat_links(
            State(self.st.clone()),
            user.clone(),
            Path((self.open_game, self.solved)),
            HeaderMap::new(),
            axum::extract::ConnectInfo(std::net::SocketAddr::from(([203, 0, 113, 9], 40000))),
            axum::Json(
                serde_json::from_value(serde_json::json!({
                    "links": links, "noAiUsed": no_ai, "expectedRevision": revision
                }))
                .unwrap(),
            ),
        )
        .await
        .map(IntoResponse::into_response)
        .unwrap_or_else(IntoResponse::into_response);
        assert_eq!(response.status(), 200);
    }

    async fn evidence(&self, user: &CurrentUser, kind: i16) -> JsonValue {
        let event_id: i32 = sqlx::query_scalar(
            r#"SELECT event.id FROM "SuspicionEvents" event
                 JOIN "UserParticipations" u ON u.participation_id = event.participation_id
                WHERE u.user_id = $1 AND u.game_id = $2 AND event.kind = $3"#,
        )
        .bind(user.id)
        .bind(self.open_game)
        .bind(kind)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        let review = super::super::cheat_evidence::suspicion_event_evidence(
            State(self.st.clone()),
            MonitorUser(self.admin.clone()),
            Path((self.open_game, event_id)),
        )
        .await
        .unwrap()
        .into_response();
        let bytes = axum::body::to_bytes(review.into_body(), usize::MAX)
            .await
            .unwrap();
        let review: JsonValue = serde_json::from_slice(&bytes).unwrap();
        match review.get("data") {
            Some(data) => data.clone(),
            None => review,
        }
    }

    async fn store_writeup(&self, user: &CurrentUser, pdf: &[u8]) {
        let (participation_id, team_id) = self.membership(user).await;
        crate::services::blob_refs::store_and_replace_writeup(
            &self.pool,
            self.st.storage.as_ref(),
            crate::services::live_roster::LiveParticipationIdentity {
                user_id: user.id,
                expected_security_stamp: "stamp",
                game_id: self.open_game,
                team_id,
                participation_id,
            },
            &format!(
                "Writeup-{}-{team_id}-{}.pdf",
                self.open_game,
                Uuid::new_v4()
            ),
            pdf,
        )
        .await
        .unwrap();
    }

    async fn set_practice_mode(&self) {
        // Git-synced events keep challenges playable after the end by default.
        sqlx::query(r#"UPDATE "Games" SET practice_mode = TRUE WHERE id = $1"#)
            .bind(self.open_game)
            .execute(&self.pool)
            .await
            .unwrap();
        super::super::scoreboard_board::invalidate_game_row_cache(self.open_game);
    }

    async fn events(&self, user: &CurrentUser) -> Vec<(i16, String, Option<i32>)> {
        let (participation_id, _) = self.membership(user).await;
        sqlx::query_as(
            r#"SELECT kind, evidence_key, challenge_id FROM "SuspicionEvents"
                WHERE participation_id = $1 ORDER BY kind, evidence_key"#,
        )
        .bind(participation_id)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn matches(&self) -> i64 {
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "AgentArtifactMatches""#)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

const CHATGPT: &str = "https://chatgpt.com/share/6708d9f0-5b7c-8008-a2d4-3f2e1c0b9a77";
const AGENT_ARTIFACT: i16 = 38;
const CONTRADICTION: i16 = 39;

#[tokio::test]
#[ignore = "requires RSCTF_TEST_DATABASE_URL"]
async fn solver_artifacts_raise_events_and_a_contradiction_in_either_order() {
    let f = fixture().await;
    f.enable_uploads().await;
    // Playable-after-the-end events are still competitions.
    f.set_practice_mode().await;

    // A clean solver records nothing.
    let clean = f.upload_solver(&f.bob, CLEAN_SOLVER).await;
    assert!(!agent_artifacts::scan_solver_upload(&f.st, clean)
        .await
        .unwrap());
    assert_eq!(f.matches().await, 0);
    assert!(f.events(&f.bob).await.is_empty());

    // Order A: artifact first (after the event ended), then "No AI used".
    let upload = f.upload_solver(&f.alice, AGENT_SOLVER).await;
    assert!(agent_artifacts::scan_solver_upload(&f.st, upload)
        .await
        .unwrap());
    let events = f.events(&f.alice).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, AGENT_ARTIFACT);
    assert!(events[0]
        .1
        .starts_with(&format!("agent-artifact:solver:{}:", f.solved)));
    assert_eq!(events[0].2, Some(f.solved));
    let score: i32 = sqlx::query_scalar(
        r#"SELECT suspicion_score FROM "Participations" p
             JOIN "UserParticipations" u ON u.participation_id = p.id
            WHERE u.user_id = $1 AND u.game_id = $2"#,
    )
    .bind(f.alice.id)
    .bind(f.open_game)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert!(score > 0, "the event contributes to the suspicion score");

    f.declare_no_ai(&f.alice, 0).await;
    let kinds = f
        .events(&f.alice)
        .await
        .into_iter()
        .map(|e| e.0)
        .collect::<Vec<_>>();
    assert_eq!(kinds, [AGENT_ARTIFACT, CONTRADICTION]);

    // Rescans and repeated scans are idempotent.
    let before = f.matches().await;
    let summary = agent_artifacts::rescan_game(&f.st, f.open_game)
        .await
        .unwrap();
    assert_eq!(summary.new_events, 0);
    assert_eq!(summary.failures, 0);
    assert_eq!(summary.solvers_scanned, 2);
    assert!(!agent_artifacts::scan_solver_upload(&f.st, upload)
        .await
        .unwrap());
    assert_eq!(f.matches().await, before);
    assert_eq!(f.events(&f.alice).await.len(), 2);

    // Order B: "No AI used" first, later switched to a chat link, then the
    // artifact upload. The earlier declaration stays in the history.
    f.declare_no_ai(&f.bob, 0).await;
    f.save_disclosure(&f.bob, &[CHATGPT], false, 1).await;
    assert!(f.events(&f.bob).await.is_empty());
    let late = f.upload_solver(&f.bob, AGENT_SOLVER).await;
    assert!(agent_artifacts::scan_solver_upload(&f.st, late)
        .await
        .unwrap());
    let kinds = f
        .events(&f.bob)
        .await
        .into_iter()
        .map(|e| e.0)
        .collect::<Vec<_>>();
    assert_eq!(kinds, [AGENT_ARTIFACT, CONTRADICTION]);

    // The monitor report lists both teams from these signals alone.
    let report = super::super::cheat::build_cheat_report(&f.st, f.open_game)
        .await
        .unwrap();
    let listed = report
        .suspicion_list
        .iter()
        .filter_map(|entry| entry["teamName"].as_str())
        .collect::<Vec<_>>();
    assert!(
        listed.contains(&"Alpha") && listed.contains(&"Beta"),
        "{listed:?}"
    );

    // The evidence view shows the matched file, signature, and declaration.
    let review = f.evidence(&f.alice, CONTRADICTION).await;
    let text = review.to_string();
    assert_eq!(review["detectorCode"], "AiDeclarationContradiction");
    assert_eq!(review["sourceStatus"], "supporting");
    assert!(
        text.contains("aiDisclosure") && text.contains("agentArtifact"),
        "{text}"
    );
    assert!(text.contains("Claude Code scratchpad path"), "{text}");
    assert!(text.contains("grind_key.txt"), "{text}");

    // Clearing the declaration afterwards cannot hide it from reviewers.
    f.save_disclosure(&f.alice, &[], false, 1).await;
    let text = f.evidence(&f.alice, CONTRADICTION).await.to_string();
    assert!(
        text.contains("aiDisclosure") && text.contains("First declared no AI"),
        "{text}"
    );
    assert!(text.contains("Cleared"), "{text}");
    f.teardown().await;
}

#[tokio::test]
#[ignore = "requires RSCTF_TEST_DATABASE_URL"]
async fn writeups_are_scanned_as_uploaded_even_after_a_quick_replacement() {
    let f = fixture().await;
    f.enable_uploads().await;
    f.set_practice_mode().await;
    let (participation_id, team_id) = f.membership(&f.alice).await;
    let artifact =
        include_bytes!("../../services/agent_artifacts/fixtures/chrome-writeup-artifact.pdf");
    let clean = include_bytes!("../../services/agent_artifacts/fixtures/chrome-writeup-clean.pdf");
    // The artifact writeup is replaced by a clean one before its scan runs;
    // replacement purges the first blob, so the scan uses the uploaded bytes.
    let first_name = format!("Writeup-{}-{team_id}-first.pdf", f.open_game);
    f.store_writeup(&f.alice, artifact).await;
    f.store_writeup(&f.alice, clean).await;
    assert!(!agent_artifacts::scan_writeup(&f.st, participation_id)
        .await
        .unwrap());
    assert!(agent_artifacts::scan_uploaded_writeup(
        &f.st,
        agent_artifacts::WriteupScan {
            game_id: f.open_game,
            participation_id,
            file_name: first_name.clone(),
            bytes: axum::body::Bytes::from_static(artifact),
            uploaded_at: Utc::now(),
        },
    )
    .await
    .unwrap());
    let events = f.events(&f.alice).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, AGENT_ARTIFACT);
    assert!(events[0].1.starts_with("agent-artifact:writeup:"));
    assert_eq!(events[0].2, None, "a writeup is not tied to one challenge");
    let (location, source, file_name): (String, String, String) = sqlx::query_as(
        r#"SELECT location, source, file_name FROM "AgentArtifactMatches"
            WHERE participation_id = $1"#,
    )
    .bind(participation_id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(
        (location.as_str(), source.as_str(), file_name.as_str()),
        ("Text", "Writeup", first_name.as_str())
    );
    assert_eq!(
        f.evidence(&f.alice, AGENT_ARTIFACT).await["sourceStatus"],
        "supporting"
    );

    // Without a matched file the event row alone is not supporting evidence.
    sqlx::query(r#"DELETE FROM "AgentArtifactMatches""#)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_ne!(
        f.evidence(&f.alice, AGENT_ARTIFACT).await["sourceStatus"],
        "supporting"
    );

    // A team never admitted to the competition is not scanned.
    let (pending_participation, _) = f.membership(&f.pending).await;
    assert!(!agent_artifacts::scan_uploaded_writeup(
        &f.st,
        agent_artifacts::WriteupScan {
            game_id: f.open_game,
            participation_id: pending_participation,
            file_name: "Writeup-pending.pdf".into(),
            bytes: axum::body::Bytes::from_static(artifact),
            uploaded_at: Utc::now(),
        },
    )
    .await
    .unwrap());
    assert_eq!(f.matches().await, 0);
    f.teardown().await;
}

#[tokio::test]
#[ignore = "requires RSCTF_TEST_DATABASE_URL"]
async fn solvers_for_solves_after_the_end_are_not_scanned() {
    let f = fixture().await;
    f.enable_uploads().await;
    f.set_practice_mode().await;
    let (participation_id, team_id) = f.membership(&f.alice).await;
    // Practice after the end: the canonical solve lands after the window.
    let mut tx = f.pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('rsctf.identity_neutral_insert', '1', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    let submission: i32 = sqlx::query_scalar(
        r#"INSERT INTO "Submissions"
             (answer, status, submit_time_utc, user_id, team_id, participation_id, game_id,
              challenge_id)
           VALUES ('flag{x}', 1, now(), $1, $2, $3, $4, $5)
        RETURNING id"#,
    )
    .bind(f.alice.id)
    .bind(team_id)
    .bind(participation_id)
    .bind(f.open_game)
    .bind(f.unsolved)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "FirstSolves" (participation_id, challenge_id, submission_id)
           VALUES ($1, $2, $3)"#,
    )
    .bind(participation_id)
    .bind(f.unsolved)
    .bind(submission)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let upload = f
        .upload_solver_for(&f.alice, f.unsolved, AGENT_SOLVER)
        .await;
    assert!(!agent_artifacts::scan_solver_upload(&f.st, upload)
        .await
        .unwrap());
    assert_eq!(f.matches().await, 0);
    assert!(f.events(&f.alice).await.is_empty());
    f.teardown().await;
}

#[tokio::test]
#[ignore = "requires RSCTF_TEST_DATABASE_URL"]
async fn late_agent_artifacts_do_not_leave_a_sealed_game_dirty() {
    let f = fixture().await;
    f.enable_uploads().await;
    // The final reconciliation sealed the event; solvers and writeups keep
    // arriving afterwards and are scanned then.
    for sql in [
        r#"UPDATE "SuspicionReconciliationState"
              SET evidence_closed_at_utc = COALESCE(evidence_closed_at_utc, clock_timestamp()),
                  sealed_at_utc = COALESCE(sealed_at_utc, clock_timestamp())
            WHERE game_id = $1"#,
        r#"UPDATE "AntiCheatReconciliationQueue"
              SET applied_generation = desired_generation,
                  final_requested_at_utc = COALESCE(final_requested_at_utc, clock_timestamp()),
                  final_applied_at_utc = COALESCE(final_applied_at_utc, clock_timestamp())
            WHERE game_id = $1"#,
        r#"UPDATE "AntiCheatReconciliationSources"
              SET applied_version = dirty_version WHERE game_id = $1"#,
    ] {
        sqlx::query(sql)
            .bind(f.open_game)
            .execute(&f.pool)
            .await
            .unwrap();
    }
    let upload = f.upload_solver(&f.alice, AGENT_SOLVER).await;
    assert!(agent_artifacts::scan_solver_upload(&f.st, upload)
        .await
        .unwrap());
    assert_eq!(f.events(&f.alice).await[0].0, AGENT_ARTIFACT);
    let (dirty_sources, clean_queue): (i64, bool) = sqlx::query_as(
        r#"SELECT (SELECT COUNT(*) FROM "AntiCheatReconciliationSources"
                    WHERE game_id = $1 AND dirty_version > applied_version),
                  (SELECT desired_generation = applied_generation
                     FROM "AntiCheatReconciliationQueue" WHERE game_id = $1)"#,
    )
    .bind(f.open_game)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(dirty_sources, 0, "nothing can reconcile a sealed game");
    assert!(clean_queue, "a sealed game's queue stays settled");
    f.teardown().await;
}
