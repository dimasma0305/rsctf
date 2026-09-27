//! Real-PostgreSQL boundaries for team AI chat links, the monitor list, and
//! the admin provider registry, through the actual handlers.

use std::str::FromStr;
use std::sync::Arc;

use axum::response::IntoResponse;
use sea_orm::SqlxPostgresConnector;
use serde_json::{json, Value as JsonValue};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::*;
use crate::app_state::AppState;
use crate::controllers::admin::ai_chat_providers::{
    delete_provider, list_providers, save_provider,
};
use crate::middlewares::privilege_authentication::AdminUser;
use crate::migrations::{test_process_application_name, Migrator, MigratorTrait};
use crate::models::internal::configs::AppConfig;
use crate::services::cache::InMemoryCache;
use crate::services::container::NoopContainerManager;
use crate::services::token::TokenService;
use crate::storage::LocalBlobStorage;
use crate::utils::enums::Role;

const CHATGPT: &str = "https://chatgpt.com/share/6708d9f0-5b7c-8008-a2d4-3f2e1c0b9a77";
const CLAUDE: &str = "https://claude.ai/share/2f1c9e4a-8b7d-4c3e-9f60-1a2b3c4d5e6f";
const COPILOT: &str = "https://copilot.microsoft.com/shares/AbCdEf123456";

pub(crate) struct Fixture {
    pub(crate) st: SharedState,
    pub(crate) pool: sqlx::PgPool,
    pub(crate) admin_pool: sqlx::PgPool,
    pub(crate) schema: String,
    pub(crate) open_game: i32,
    pub(crate) off_game: i32,
    pub(crate) closed_game: i32,
    pub(crate) solved: i32,
    pub(crate) unsolved: i32,
    pub(crate) attack_defense: i32,
    pub(crate) closed_solved: i32,
    pub(crate) off_solved: i32,
    pub(crate) alice: CurrentUser,
    pub(crate) bob: CurrentUser,
    pub(crate) outsider: CurrentUser,
    pub(crate) pending: CurrentUser,
    pub(crate) admin: CurrentUser,
}

fn user(id: Uuid, name: &str, role: Role) -> CurrentUser {
    CurrentUser {
        id,
        role,
        name: name.into(),
        security_stamp: "stamp".into(),
    }
}

async fn insert_game(
    tx: &mut sqlx::PgConnection,
    title: &str,
    enabled: bool,
    writeup_deadline: DateTime<Utc>,
) -> i32 {
    let now = Utc::now();
    let (public_key, private_key) = crate::utils::crypto_utils::generate_game_keypair();
    sqlx::query_scalar(
        r#"INSERT INTO "Games"
             (title, public_key, private_key, hidden, practice_mode, summary, content,
              accept_without_review, allow_user_submissions, writeup_required,
              team_member_count_limit, container_count_limit, start_time_utc, end_time_utc,
              writeup_deadline, writeup_note, blood_bonus_value, ad_allow_snapshot_download,
              ad_scoring_paused, ai_chat_links_enabled)
           VALUES ($1, $2, $3, FALSE, FALSE, '', '', TRUE, FALSE, FALSE, 0, 3, $4, $5, $6,
                   '', 0, TRUE, FALSE, $7)
        RETURNING id"#,
    )
    .bind(title)
    .bind(public_key)
    .bind(private_key)
    .bind(now - chrono::Duration::hours(3))
    .bind(now - chrono::Duration::hours(1))
    .bind(writeup_deadline)
    .bind(enabled)
    .fetch_one(&mut *tx)
    .await
    .unwrap()
}

async fn insert_challenge(
    tx: &mut sqlx::PgConnection,
    game_id: i32,
    title: &str,
    kind: ChallengeType,
) -> i32 {
    sqlx::query_scalar(
        r#"INSERT INTO "GameChallenges"
             (game_id, title, content, category, "Type", is_enabled, submission_limit,
              accepted_count, submission_count, review_status, build_status,
              enable_traffic_capture, enable_shared_container, original_score, min_score_rate,
              difficulty, score_curve, ad_allow_egress, ad_allow_self_reset,
              ad_ssh_requires_flag, ad_self_hosted)
           VALUES ($1, $2, '', 0, $3, TRUE, 0, 0, 0, 0, 0, FALSE, FALSE, 500, 0.25, 5, 0,
                   FALSE, FALSE, FALSE, FALSE)
        RETURNING id"#,
    )
    .bind(game_id)
    .bind(title)
    .bind(kind as i16)
    .fetch_one(&mut *tx)
    .await
    .unwrap()
}

async fn insert_user(tx: &mut sqlx::PgConnection, name: &str, role: Role) -> Uuid {
    let id = Uuid::new_v4();
    let now = Utc::now();
    sqlx::query(
        r#"INSERT INTO "AspNetUsers"
             (id, user_name, normalized_user_name, email_confirmed, security_stamp,
              phone_number_confirmed, two_factor_enabled, lockout_enabled, access_failed_count,
              role, ip, last_signed_in_utc, last_visited_utc, register_time_utc, bio,
              real_name, std_number, exercise_visible)
           VALUES ($1, $2, upper($2), TRUE, 'stamp', FALSE, FALSE, FALSE, 0, $3, '', $4, $4, $4,
                   '', '', '', TRUE)"#,
    )
    .bind(id)
    .bind(name)
    .bind(role as i16)
    .bind(now)
    .execute(&mut *tx)
    .await
    .unwrap();
    id
}

async fn insert_team(tx: &mut sqlx::PgConnection, name: &str, captain: Uuid) -> i32 {
    let team: i32 = sqlx::query_scalar(
        r#"INSERT INTO "Teams" (name, locked, invite_token, captain_id)
           VALUES ($1, FALSE, $2, $3) RETURNING id"#,
    )
    .bind(name)
    .bind(crate::utils::codec::random_hex(16))
    .bind(captain)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES ($1, $2)"#)
        .bind(team)
        .bind(captain)
        .execute(&mut *tx)
        .await
        .unwrap();
    team
}

async fn join(
    tx: &mut sqlx::PgConnection,
    game_id: i32,
    team_id: i32,
    user_id: Uuid,
    status: ParticipationStatus,
) -> i32 {
    let participation: i32 = sqlx::query_scalar(
        r#"INSERT INTO "Participations" (status, token, game_id, team_id, suspicion_score)
           VALUES ($1, 'token', $2, $3, 0) RETURNING id"#,
    )
    .bind(status as i16)
    .bind(game_id)
    .bind(team_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "UserParticipations" (user_id, game_id, team_id, participation_id)
           VALUES ($1, $2, $3, $4)"#,
    )
    .bind(user_id)
    .bind(game_id)
    .bind(team_id)
    .bind(participation)
    .execute(&mut *tx)
    .await
    .unwrap();
    participation
}

async fn solve(
    tx: &mut sqlx::PgConnection,
    game_id: i32,
    team_id: i32,
    participation: i32,
    challenge: i32,
    user_id: Uuid,
) {
    let submission: i32 = sqlx::query_scalar(
        r#"INSERT INTO "Submissions"
             (answer, status, submit_time_utc, user_id, team_id, participation_id, game_id, challenge_id)
           VALUES ('flag{x}', 1, $1, $2, $3, $4, $5, $6)
        RETURNING id"#,
    )
    .bind(Utc::now() - chrono::Duration::minutes(90))
    .bind(user_id)
    .bind(team_id)
    .bind(participation)
    .bind(game_id)
    .bind(challenge)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    // A real accepted solve always records its canonical first solve.
    sqlx::query(
        r#"INSERT INTO "FirstSolves" (participation_id, challenge_id, submission_id)
           VALUES ($1, $2, $3)"#,
    )
    .bind(participation)
    .bind(challenge)
    .bind(submission)
    .execute(&mut *tx)
    .await
    .unwrap();
}

pub(crate) async fn fixture() -> Fixture {
    fixture_with(false).await
}

async fn fixture_with(required: bool) -> Fixture {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let application_name = test_process_application_name();
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(
            PgConnectOptions::from_str(&database_url)
                .unwrap()
                .application_name(application_name),
        )
        .await
        .unwrap();
    let schema = format!("rsctf_ai_chats_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin_pool)
        .await
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .connect_with(
            PgConnectOptions::from_str(&database_url)
                .unwrap()
                .application_name(application_name)
                .options([("search_path", schema.as_str())]),
        )
        .await
        .unwrap();
    let db = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
    Migrator::up(&db, None).await.unwrap();
    let st = AppState::new(
        db,
        Arc::new(AppConfig::default()),
        Arc::new(InMemoryCache::new()),
        Arc::new(LocalBlobStorage::new(
            std::env::temp_dir().join(format!("rsctf-ai-chats-{schema}")),
        )),
        TokenService::new("0123456789abcdef0123456789abcdef", 60),
        Arc::new(NoopContainerManager),
    );

    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('rsctf.identity_neutral_insert', '1', true)")
        .execute(&mut *tx)
        .await
        .unwrap();
    // The process-wide game row cache is keyed by id: isolate each schema's ids.
    sqlx::query("SELECT setval(pg_get_serial_sequence('\"Games\"', 'id'), $1)")
        .bind(1_000_000 + i64::from(rand::random::<u32>() % 1_000_000_000))
        .execute(&mut *tx)
        .await
        .unwrap();
    let now = Utc::now();
    let open_game = insert_game(&mut tx, "Open", true, now + chrono::Duration::hours(1)).await;
    sqlx::query(r#"UPDATE "Games" SET ai_chat_links_required = $2 WHERE id = $1"#)
        .bind(open_game)
        .bind(required)
        .execute(&mut *tx)
        .await
        .unwrap();
    let off_game = insert_game(&mut tx, "Off", false, now + chrono::Duration::hours(1)).await;
    let closed_game =
        insert_game(&mut tx, "Closed", true, now - chrono::Duration::minutes(30)).await;
    let solved = insert_challenge(
        &mut tx,
        open_game,
        "Warmup",
        ChallengeType::StaticAttachment,
    )
    .await;
    let unsolved =
        insert_challenge(&mut tx, open_game, "Hard", ChallengeType::StaticAttachment).await;
    let attack_defense =
        insert_challenge(&mut tx, open_game, "Service", ChallengeType::AttackDefense).await;
    let closed_solved =
        insert_challenge(&mut tx, closed_game, "Old", ChallengeType::StaticAttachment).await;
    let off_solved =
        insert_challenge(&mut tx, off_game, "Off", ChallengeType::StaticAttachment).await;

    let alice = insert_user(&mut tx, "alice", Role::User).await;
    let bob = insert_user(&mut tx, "bob", Role::User).await;
    let outsider = insert_user(&mut tx, "outsider", Role::User).await;
    let pending = insert_user(&mut tx, "pending", Role::User).await;
    let admin = insert_user(&mut tx, "admin", Role::Admin).await;
    let alpha = insert_team(&mut tx, "Alpha", alice).await;
    let beta = insert_team(&mut tx, "Beta", bob).await;
    let gamma = insert_team(&mut tx, "Gamma", pending).await;

    let alpha_open = join(
        &mut tx,
        open_game,
        alpha,
        alice,
        ParticipationStatus::Accepted,
    )
    .await;
    let beta_open = join(&mut tx, open_game, beta, bob, ParticipationStatus::Accepted).await;
    join(
        &mut tx,
        open_game,
        gamma,
        pending,
        ParticipationStatus::Pending,
    )
    .await;
    let alpha_off = join(
        &mut tx,
        off_game,
        alpha,
        alice,
        ParticipationStatus::Accepted,
    )
    .await;
    let alpha_closed = join(
        &mut tx,
        closed_game,
        alpha,
        alice,
        ParticipationStatus::Accepted,
    )
    .await;
    solve(&mut tx, open_game, alpha, alpha_open, solved, alice).await;
    solve(&mut tx, open_game, beta, beta_open, solved, bob).await;
    solve(&mut tx, off_game, alpha, alpha_off, off_solved, alice).await;
    solve(
        &mut tx,
        closed_game,
        alpha,
        alpha_closed,
        closed_solved,
        alice,
    )
    .await;
    tx.commit().await.unwrap();

    Fixture {
        st,
        pool,
        admin_pool,
        schema,
        open_game,
        off_game,
        closed_game,
        solved,
        unsolved,
        attack_defense,
        closed_solved,
        off_solved,
        alice: user(alice, "alice", Role::User),
        bob: user(bob, "bob", Role::User),
        outsider: user(outsider, "outsider", Role::User),
        pending: user(pending, "pending", Role::User),
        admin: user(admin, "admin", Role::Admin),
    }
}

pub(crate) async fn body(response: Response) -> JsonValue {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

pub(crate) fn status(result: AppResult<Response>) -> u16 {
    match result {
        Ok(response) => response.status().as_u16(),
        Err(error) => error.into_response().status().as_u16(),
    }
}

pub(crate) async fn error_title(result: AppResult<Response>) -> String {
    let response = match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };
    body(response).await["title"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

impl Fixture {
    async fn get(&self, user: &CurrentUser, game: i32, challenge: i32) -> AppResult<Response> {
        get_ai_chat_links(
            State(self.st.clone()),
            user.clone(),
            Path((game, challenge)),
        )
        .await
    }

    async fn put(
        &self,
        user: &CurrentUser,
        game: i32,
        challenge: i32,
        links: &[&str],
        revision: i32,
    ) -> AppResult<Response> {
        self.save(
            user,
            game,
            challenge,
            json!({ "links": links, "expectedRevision": revision }),
        )
        .await
    }

    async fn save(
        &self,
        user: &CurrentUser,
        game: i32,
        challenge: i32,
        body: JsonValue,
    ) -> AppResult<Response> {
        let model: SaveAiChatLinks = serde_json::from_value(body).unwrap();
        save_ai_chat_links(
            State(self.st.clone()),
            user.clone(),
            Path((game, challenge)),
            axum::http::HeaderMap::new(),
            axum::extract::ConnectInfo(std::net::SocketAddr::from(([203, 0, 113, 7], 40000))),
            axum::Json(model),
        )
        .await
    }

    async fn provider(&self, key: &str, value: JsonValue) -> AppResult<Response> {
        save_provider(
            State(self.st.clone()),
            AdminUser(self.admin.clone()),
            Path(key.to_string()),
            axum::Json(serde_json::from_value(value).unwrap()),
        )
        .await
    }

    async fn monitor(&self) -> JsonValue {
        self.monitor_status(None).await
    }

    async fn monitor_status(&self, status: Option<&str>) -> JsonValue {
        let query: AiChatMonitorQuery =
            serde_json::from_value(json!({ "status": status })).unwrap();
        body(
            list_ai_chat_links(
                State(self.st.clone()),
                MonitorUser(self.admin.clone()),
                Path(self.open_game),
                Query(query),
            )
            .await
            .unwrap(),
        )
        .await
    }

    async fn monitor_status_result(&self, status: Option<&str>) -> AppResult<Response> {
        let query: AiChatMonitorQuery =
            serde_json::from_value(json!({ "status": status })).unwrap();
        list_ai_chat_links(
            State(self.st.clone()),
            MonitorUser(self.admin.clone()),
            Path(self.open_game),
            Query(query),
        )
        .await
    }

    pub(crate) async fn teardown(self) {
        self.pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
            .execute(&self.admin_pool)
            .await
            .unwrap();
        self.admin_pool.close().await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn team_links_are_solve_gated_team_scoped_and_provider_checked() {
    let f = fixture().await;

    // A solved team sees an empty, editable record and the enabled providers.
    let state = body(f.get(&f.alice, f.open_game, f.solved).await.unwrap()).await;
    assert_eq!(state["solved"], true);
    assert_eq!(state["editable"], true);
    assert_eq!(state["revision"], 0);
    assert_eq!(state["maxLinks"], 5);
    assert!(state["providers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["key"] == "chatgpt"));

    // Save two links; the fragment is stripped and the saver is recorded.
    let saved = body(
        f.put(
            &f.alice,
            f.open_game,
            f.solved,
            &[CHATGPT, &format!("{CLAUDE}#top")],
            0,
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(saved["revision"], 1);
    assert_eq!(saved["submittedBy"], "alice");
    assert_eq!(saved["links"][0]["providerKey"], "chatgpt");
    assert_eq!(saved["links"][1]["url"], CLAUDE);
    assert_eq!(saved["links"][1]["providerLabel"], "Claude");

    // Stale revision, another team, unsolved, A&D, outsider, pending: all rejected.
    assert_eq!(
        status(f.put(&f.alice, f.open_game, f.solved, &[CHATGPT], 0).await),
        409
    );
    let other = body(f.get(&f.bob, f.open_game, f.solved).await.unwrap()).await;
    assert_eq!(other["revision"], 0, "another team never sees these links");
    assert!(other["links"].as_array().unwrap().is_empty());
    assert_eq!(
        status(
            f.put(&f.alice, f.open_game, f.unsolved, &[CHATGPT], 0)
                .await
        ),
        400
    );
    assert_eq!(
        status(f.get(&f.alice, f.open_game, f.attack_defense).await),
        404
    );
    assert_eq!(
        status(
            f.put(&f.alice, f.open_game, f.attack_defense, &[CHATGPT], 0)
                .await
        ),
        404
    );
    assert_eq!(status(f.get(&f.outsider, f.open_game, f.solved).await), 400);
    assert_eq!(status(f.get(&f.pending, f.open_game, f.solved).await), 400);

    // Unsupported, insecure, duplicated, and oversized submissions.
    let title = error_title(
        f.put(
            &f.alice,
            f.open_game,
            f.solved,
            &["https://example.test/share/abc"],
            1,
        )
        .await,
    )
    .await;
    assert!(title.starts_with("Link 1:"), "{title}");
    assert_eq!(
        status(
            f.put(
                &f.alice,
                f.open_game,
                f.solved,
                &["http://chatgpt.com/share/abcdefgh12"],
                1
            )
            .await
        ),
        400
    );
    assert_eq!(
        status(
            f.put(&f.alice, f.open_game, f.solved, &[CLAUDE, CLAUDE], 1)
                .await
        ),
        400
    );
    assert_eq!(
        status(
            f.put(&f.alice, f.open_game, f.solved, &[CLAUDE; 6], 1)
                .await
        ),
        400
    );

    // Feature off and a closed window.
    assert_eq!(status(f.get(&f.alice, f.off_game, f.off_solved).await), 404);
    assert_eq!(
        status(
            f.put(&f.alice, f.off_game, f.off_solved, &[CHATGPT], 0)
                .await
        ),
        404
    );
    let closed = body(
        f.get(&f.alice, f.closed_game, f.closed_solved)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(closed["editable"], false);
    assert_eq!(
        status(
            f.put(&f.alice, f.closed_game, f.closed_solved, &[CHATGPT], 0)
                .await
        ),
        400
    );

    // Monitors see the record with the provider marked active.
    // Beta solved too but disclosed nothing, so it is listed as Missing.
    let page = f.monitor().await;
    assert_eq!(page["total"], 2);
    assert_eq!(page["required"], false);
    assert_eq!(page["items"][0]["status"], "Links");
    assert_eq!(page["items"][1]["status"], "Missing");
    assert_eq!(page["items"][1]["teamName"], "Beta");
    assert_eq!(page["items"][0]["teamName"], "Alpha");
    assert_eq!(page["items"][0]["challengeTitle"], "Warmup");
    assert_eq!(page["items"][0]["category"], "Misc");
    assert_eq!(page["items"][0]["links"][0]["providerActive"], true);

    // Blocking a built-in rejects new links but keeps saved ones visible.
    assert_eq!(
        status(f.provider("chatgpt", json!({ "enabled": false })).await),
        200
    );
    let page = f.monitor().await;
    assert_eq!(page["items"][0]["links"][0]["providerActive"], false);
    assert_eq!(page["items"][0]["links"][0]["providerLabel"], "ChatGPT");
    assert_eq!(
        status(f.put(&f.alice, f.open_game, f.solved, &[CHATGPT], 1).await),
        400
    );

    // A custom provider extends the matcher.
    assert_eq!(
        status(f.put(&f.alice, f.open_game, f.solved, &[COPILOT], 1).await),
        400
    );
    let created = body(
        f.provider(
            "copilot",
            json!({
                "enabled": true,
                "label": "Microsoft Copilot",
                "pattern": r"https://copilot\.microsoft\.com/shares/[A-Za-z0-9_-]{6,128}"
            }),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(created["builtin"], false);
    let saved = body(
        f.put(&f.alice, f.open_game, f.solved, &[COPILOT, CLAUDE], 1)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(saved["revision"], 2);
    assert_eq!(saved["links"][0]["providerLabel"], "Microsoft Copilot");

    // An empty set removes the record.
    let cleared = body(
        f.put(&f.alice, f.open_game, f.solved, &[], 2)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(cleared["revision"], 0);
    assert_eq!(
        cleared["pending"], false,
        "nothing is pending when not required"
    );
    assert_eq!(f.monitor_status(Some("Links")).await["total"], 0);
    assert_eq!(f.monitor_status(Some("Missing")).await["total"], 2);

    f.teardown().await;
}

#[path = "ai_chat_providers_tests.rs"]
mod providers;

#[derive(sqlx::FromRow, Debug)]
struct EventTelemetry {
    action: String,
    revision: i32,
    added_urls: sqlx::types::Json<Vec<String>>,
    removed_urls: sqlx::types::Json<Vec<String>>,
    previous_declared_no_ai: bool,
    declared_no_ai: bool,
    seconds_since_solve: Option<i64>,
    remote_ip_hash: Option<Vec<u8>>,
    user_id: Option<Uuid>,
}

async fn telemetry(f: &Fixture) -> Vec<EventTelemetry> {
    sqlx::query_as::<_, EventTelemetry>(
        r#"SELECT action, revision, added_urls, removed_urls, previous_declared_no_ai,
                  declared_no_ai, seconds_since_solve, remote_ip_hash, user_id
             FROM "AiChatLinkEvents" WHERE game_id = $1 ORDER BY occurred_at, id"#,
    )
    .bind(f.open_game)
    .fetch_all(&f.pool)
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn required_disclosure_records_every_edit_with_server_timing() {
    let f = fixture_with(true).await;

    // A required event reports the solve as pending until something is disclosed.
    let state = body(f.get(&f.alice, f.open_game, f.solved).await.unwrap()).await;
    assert_eq!(state["required"], true);
    assert_eq!(state["pending"], true);
    assert!(state["solvedAt"].as_i64().is_some());
    assert!(state["firstDisclosedAt"].is_null());
    let pending = body(
        pending_ai_chat_links(State(f.st.clone()), f.alice.clone(), Path(f.open_game))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(pending["required"], true);
    assert_eq!(pending["challengeIds"], json!([f.solved]));

    // Links and a "no AI" declaration are mutually exclusive.
    let both = json!({ "links": [CHATGPT], "noAiUsed": true, "expectedRevision": 0 });
    assert_eq!(
        status(f.save(&f.alice, f.open_game, f.solved, both).await),
        400
    );
    assert!(
        telemetry(&f).await.is_empty(),
        "rejected writes leave no telemetry"
    );

    // Declare no AI: disclosed, not pending, one Created event with timing.
    let declared = json!({ "links": [], "noAiUsed": true, "expectedRevision": 0 });
    let state = body(
        f.save(&f.alice, f.open_game, f.solved, declared.clone())
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(state["declaredNoAi"], true);
    assert_eq!(state["pending"], false);
    assert_eq!(state["revision"], 1);
    assert!(state["firstDisclosedAt"].as_i64().is_some());
    // An identical save is a no-op: no revision bump and no telemetry row.
    let replay = json!({ "links": [], "noAiUsed": true, "expectedRevision": 1 });
    let state = body(
        f.save(&f.alice, f.open_game, f.solved, replay)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(state["revision"], 1);
    let events = telemetry(&f).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].action, "Created");
    assert!(events[0].declared_no_ai);
    assert_eq!(events[0].user_id, Some(f.alice.id));
    let delay = events[0].seconds_since_solve.unwrap();
    assert!(
        (5390..5460).contains(&delay),
        "solved 90 minutes earlier, got {delay}"
    );
    assert_eq!(events[0].remote_ip_hash.as_ref().map(Vec::len), Some(32));

    // Replace the declaration with a link, then swap the link: both are edits.
    body(
        f.put(&f.alice, f.open_game, f.solved, &[CHATGPT], 1)
            .await
            .unwrap(),
    )
    .await;
    let state = body(
        f.put(&f.alice, f.open_game, f.solved, &[CLAUDE], 2)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(state["editCount"], 2);
    assert_eq!(state["declaredNoAi"], false);
    // Clearing makes the required disclosure pending again.
    let state = body(
        f.put(&f.alice, f.open_game, f.solved, &[], 3)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(state["pending"], true);
    assert_eq!(state["revision"], 0);

    let events = telemetry(&f).await;
    let actions = events
        .iter()
        .map(|event| event.action.as_str())
        .collect::<Vec<_>>();
    assert_eq!(actions, ["Created", "Edited", "Edited", "Cleared"]);
    assert!(events[1].previous_declared_no_ai && !events[1].declared_no_ai);
    assert_eq!(events[1].added_urls.0, [CHATGPT]);
    assert!(events[1].removed_urls.0.is_empty());
    assert_eq!(events[2].added_urls.0, [CLAUDE]);
    assert_eq!(events[2].removed_urls.0, [CHATGPT]);
    assert_eq!(events[3].removed_urls.0, [CLAUDE]);
    assert_eq!(events[3].revision, 0);
    assert!(events.iter().all(|event| event.remote_ip_hash.is_some()));

    // Monitors see both teams as Missing, with Alpha's history and timing.
    let page = f.monitor_status(Some("Missing")).await;
    assert_eq!(page["required"], true);
    assert_eq!(page["total"], 2);
    let alpha = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["teamName"] == "Alpha")
        .unwrap()
        .clone();
    assert_eq!(alpha["editCount"], 2);
    assert_eq!(alpha["eventCount"], 4);
    assert!(alpha["delaySeconds"].as_i64().unwrap() >= 5390);
    assert!(alpha["updatedAt"].is_null());
    let history = body(
        list_ai_chat_link_events(
            State(f.st.clone()),
            MonitorUser(f.admin.clone()),
            Path((
                f.open_game,
                alpha["participationId"].as_i64().unwrap() as i32,
                f.solved,
            )),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(history["truncated"], false);
    assert_eq!(history["items"].as_array().unwrap().len(), 4);
    assert_eq!(history["items"][2]["added"], json!([CLAUDE]));
    assert_eq!(history["items"][2]["removed"], json!([CHATGPT]));
    assert_eq!(history["items"][2]["previousLinks"], json!([CHATGPT]));
    assert_eq!(history["items"][0]["userName"], "alice");
    assert_eq!(
        history["items"][0]["networkHint"].as_str().unwrap().len(),
        12
    );
    assert_eq!(status(f.monitor_status_result(Some("Bogus")).await), 400);

    f.teardown().await;
}
