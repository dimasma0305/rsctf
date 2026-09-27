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

struct Fixture {
    st: SharedState,
    pool: sqlx::PgPool,
    admin_pool: sqlx::PgPool,
    schema: String,
    open_game: i32,
    off_game: i32,
    closed_game: i32,
    solved: i32,
    unsolved: i32,
    attack_defense: i32,
    closed_solved: i32,
    off_solved: i32,
    alice: CurrentUser,
    bob: CurrentUser,
    outsider: CurrentUser,
    pending: CurrentUser,
    admin: CurrentUser,
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
    sqlx::query(
        r#"INSERT INTO "Submissions"
             (answer, status, submit_time_utc, user_id, team_id, participation_id, game_id, challenge_id)
           VALUES ('flag{x}', 1, $1, $2, $3, $4, $5, $6)"#,
    )
    .bind(Utc::now() - chrono::Duration::minutes(90))
    .bind(user_id)
    .bind(team_id)
    .bind(participation)
    .bind(game_id)
    .bind(challenge)
    .execute(&mut *tx)
    .await
    .unwrap();
}

async fn fixture() -> Fixture {
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
    let now = Utc::now();
    let open_game = insert_game(&mut tx, "Open", true, now + chrono::Duration::hours(1)).await;
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

async fn body(response: Response) -> JsonValue {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn status(result: AppResult<Response>) -> u16 {
    match result {
        Ok(response) => response.status().as_u16(),
        Err(error) => error.into_response().status().as_u16(),
    }
}

async fn error_title(result: AppResult<Response>) -> String {
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
        let model: SaveAiChatLinks =
            serde_json::from_value(json!({ "links": links, "expectedRevision": revision }))
                .unwrap();
        save_ai_chat_links(
            State(self.st.clone()),
            user.clone(),
            Path((game, challenge)),
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
        let query: AiChatMonitorQuery = serde_json::from_value(json!({})).unwrap();
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

    async fn teardown(self) {
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
    let page = f.monitor().await;
    assert_eq!(page["total"], 1);
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
    assert_eq!(f.monitor().await["total"], 0);

    f.teardown().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn provider_registry_protects_builtins_and_validates_custom_rules() {
    let f = fixture().await;
    assert_eq!(
        status(
            f.provider(
                "chatgpt",
                json!({ "enabled": false, "label": "x", "pattern": "https://x\\.test/a" })
            )
            .await
        ),
        400,
        "built-ins accept only the enabled switch"
    );
    assert_eq!(
        status(
            f.provider(
                "bad key",
                json!({ "enabled": true, "label": "x", "pattern": "https://x\\.test/a" })
            )
            .await
        ),
        400
    );
    for pattern in [
        r"https://.*",
        "http://poe\\.com/s/[a-z]+",
        r"https://poe\.com/s/(?=a)",
        r"^https://poe\.com/s/a$",
    ] {
        assert_eq!(
            status(
                f.provider(
                    "poe",
                    json!({ "enabled": true, "label": "Poe", "pattern": pattern })
                )
                .await
            ),
            400,
            "{pattern}"
        );
    }
    assert_eq!(
        status(f.provider("poe", json!({ "enabled": true, "label": "Poe", "pattern": r"https://poe\.com/s/[A-Za-z0-9_-]{6,64}" })).await),
        200
    );
    // Updating keeps one row; the cap counts custom providers only.
    assert_eq!(
        status(f.provider("poe", json!({ "enabled": false, "label": "Poe", "pattern": r"https://poe\.com/s/[A-Za-z0-9_-]{6,64}" })).await),
        200
    );
    for index in 1..32 {
        let key = format!("custom-{index}");
        assert_eq!(
            status(f.provider(&key, json!({ "enabled": true, "label": key, "pattern": r"https://example\.test/share/[a-z]{4}" })).await),
            200
        );
    }
    let title = error_title(
        f.provider("one-too-many", json!({ "enabled": true, "label": "x", "pattern": r"https://example\.test/share/[a-z]{4}" }))
            .await,
    )
    .await;
    assert!(title.contains("At most 32"), "{title}");

    let list = body(
        list_providers(State(f.st.clone()), AdminUser(f.admin.clone()))
            .await
            .unwrap(),
    )
    .await;
    let providers = list["providers"].as_array().unwrap();
    assert_eq!(
        providers.len(),
        crate::services::ai_chat_links::BUILTIN_PROVIDERS.len() + 32
    );
    assert!(providers
        .iter()
        .any(|p| p["key"] == "poe" && p["enabled"] == false));
    assert!(providers
        .iter()
        .filter(|p| p["builtin"] == true)
        .all(|p| !p["examples"].as_array().unwrap().is_empty()));

    assert_eq!(
        status(
            delete_provider(
                State(f.st.clone()),
                AdminUser(f.admin.clone()),
                Path("chatgpt".into())
            )
            .await
        ),
        400
    );
    assert_eq!(
        status(
            delete_provider(
                State(f.st.clone()),
                AdminUser(f.admin.clone()),
                Path("poe".into())
            )
            .await
        ),
        200
    );
    assert_eq!(
        status(
            delete_provider(
                State(f.st.clone()),
                AdminUser(f.admin.clone()),
                Path("poe".into())
            )
            .await
        ),
        404
    );
    let _ = (&f.bob, f.off_game, f.closed_game);
    f.teardown().await;
}
