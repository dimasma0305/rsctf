//! Admin container-instance listing + destroy + stats — split from admin/mod.rs.
use super::*;
use crate::models::data::container;

/// RSCTF `ChallengeModel` (nested challenge reference).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeModel {
    pub id: i32,
    pub title: String,
    pub category: ChallengeCategory,
}

/// What owns a managed container when no concrete team can be attached to it.
///
/// A shared challenge is intentionally teamless: one platform-launched workload
/// serves every participant. Keeping that distinct from an unknown owner prevents
/// the admin UI from inventing a team or hiding useful ownership information.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ContainerOwnerKind {
    Team,
    Shared,
    AdminTest,
    Exercise,
    Unassigned,
}

/// RSCTF `ContainerInstanceModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerInstanceModel {
    pub team: Option<TeamModel>,
    pub challenge: Option<ChallengeModel>,
    pub owner_kind: ContainerOwnerKind,
    pub owner_name: Option<String>,
    pub image: String,
    pub container_guid: Uuid,
    pub container_id: String,
    #[serde(with = "crate::utils::datetime::millis")]
    pub started_at: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub expect_stop_at: DateTime<Utc>,
    pub ip: String,
    pub port: i32,
    pub is_proxy: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct ContainerInstanceRow {
    team_id: Option<i32>,
    team_name: Option<String>,
    team_avatar_hash: Option<String>,
    challenge_id: Option<i32>,
    challenge_title: Option<String>,
    challenge_category: Option<i16>,
    owner_kind: String,
    owner_name: Option<String>,
    image: String,
    container_guid: Uuid,
    container_id: String,
    started_at: DateTime<Utc>,
    expect_stop_at: DateTime<Utc>,
    ip: String,
    port: i32,
    is_proxy: bool,
}

const INSTANCE_PAGE_SQL: &str = r#"
    SELECT COALESCE(game_team.id, service_team.id) AS team_id,
           COALESCE(game_team.name, service_team.name) AS team_name,
           COALESCE(game_team.avatar_hash, service_team.avatar_hash) AS team_avatar_hash,
           COALESCE(
               game_challenge.id,
               service_challenge.id,
               shared_challenge.id,
               test_challenge.id,
               exercise_challenge.id
           ) AS challenge_id,
           COALESCE(
               game_challenge.title,
               service_challenge.title,
               shared_challenge.title,
               test_challenge.title,
               exercise_challenge.title
           ) AS challenge_title,
           COALESCE(
               game_challenge.category,
               service_challenge.category,
               shared_challenge.category,
               test_challenge.category,
               exercise_challenge.category
           ) AS challenge_category,
           CASE
               WHEN game_team.id IS NOT NULL OR service_team.id IS NOT NULL THEN 'Team'
               WHEN shared_challenge.id IS NOT NULL THEN 'Shared'
               WHEN test_challenge.id IS NOT NULL THEN 'AdminTest'
               WHEN exercise_instance.id IS NOT NULL THEN 'Exercise'
               ELSE 'Unassigned'
           END AS owner_kind,
           CASE
               WHEN exercise_instance.id IS NOT NULL
               THEN COALESCE(exercise_user.user_name, NULLIF(exercise_user.real_name, ''))
               ELSE NULL
           END AS owner_name,
           container.image,
           container.id AS container_guid,
           container.container_id,
           container.started_at,
           container.expect_stop_at,
           COALESCE(container.public_ip, container.ip) AS ip,
           COALESCE(container.public_port, container.port) AS port,
           container.is_proxy
      FROM "Containers" container
 LEFT JOIN "GameInstances" game_instance
        ON game_instance.id = container.game_instance_id
 LEFT JOIN "GameChallenges" game_challenge
        ON game_challenge.id = game_instance.challenge_id
 LEFT JOIN "Participations" game_participation
        ON game_participation.id = game_instance.participation_id
 LEFT JOIN "Teams" game_team
        ON game_team.id = game_participation.team_id
 LEFT JOIN LATERAL (
               SELECT service.id,
                      service.participation_id,
                      service.challenge_id
                 FROM "AdTeamServices" service
                WHERE service.id = container.ad_team_service_id
                   OR (
                       container.ad_team_service_id IS NULL
                       AND service.container_id = container.container_id
                   )
                ORDER BY (service.id = container.ad_team_service_id) DESC, service.id
                LIMIT 1
           ) service ON TRUE
 LEFT JOIN "GameChallenges" service_challenge
        ON service_challenge.id = service.challenge_id
 LEFT JOIN "Participations" service_participation
        ON service_participation.id = service.participation_id
 LEFT JOIN "Teams" service_team
        ON service_team.id = service_participation.team_id
 LEFT JOIN LATERAL (
               SELECT challenge.id, challenge.title, challenge.category
                 FROM "GameChallenges" challenge
                WHERE challenge.shared_container_id = container.id
                ORDER BY challenge.id
                LIMIT 1
           ) shared_challenge ON TRUE
 LEFT JOIN LATERAL (
               SELECT challenge.id, challenge.title, challenge.category
                 FROM "GameChallenges" challenge
                WHERE challenge.test_container_id = container.id
                ORDER BY challenge.id
                LIMIT 1
           ) test_challenge ON TRUE
 LEFT JOIN LATERAL (
               SELECT instance.id, instance.exercise_id, instance.user_id
                 FROM "ExerciseInstances" instance
                WHERE instance.id = container.exercise_instance_id
                   OR (
                       container.exercise_instance_id IS NULL
                       AND instance.container_id = container.id
                   )
                ORDER BY (instance.id = container.exercise_instance_id) DESC, instance.id
                LIMIT 1
           ) exercise_instance ON TRUE
 LEFT JOIN "ExerciseChallenges" exercise_challenge
        ON exercise_challenge.id = exercise_instance.exercise_id
 LEFT JOIN "AspNetUsers" exercise_user
        ON exercise_user.id = exercise_instance.user_id
  ORDER BY container.started_at, container.id
     LIMIT $1 OFFSET $2
"#;

fn owner_kind(value: &str) -> AppResult<ContainerOwnerKind> {
    match value {
        "Team" => Ok(ContainerOwnerKind::Team),
        "Shared" => Ok(ContainerOwnerKind::Shared),
        "AdminTest" => Ok(ContainerOwnerKind::AdminTest),
        "Exercise" => Ok(ContainerOwnerKind::Exercise),
        "Unassigned" => Ok(ContainerOwnerKind::Unassigned),
        _ => Err(AppError::internal("Unknown container owner kind")),
    }
}

fn challenge_category(value: i16) -> AppResult<ChallengeCategory> {
    <ChallengeCategory as sea_orm::ActiveEnum>::try_from_value(&value)
        .map_err(|error| AppError::internal(error.to_string()))
}

fn project_instance(row: ContainerInstanceRow) -> AppResult<ContainerInstanceModel> {
    let team = match (row.team_id, row.team_name) {
        (Some(id), Some(name)) => Some(TeamModel {
            id,
            name,
            avatar: row
                .team_avatar_hash
                .map(|hash| format!("/assets/{hash}/avatar")),
        }),
        _ => None,
    };
    let challenge = match (
        row.challenge_id,
        row.challenge_title,
        row.challenge_category,
    ) {
        (Some(id), Some(title), Some(category)) => Some(ChallengeModel {
            id,
            title,
            category: challenge_category(category)?,
        }),
        _ => None,
    };

    Ok(ContainerInstanceModel {
        team,
        challenge,
        owner_kind: owner_kind(&row.owner_kind)?,
        owner_name: row.owner_name,
        image: row.image,
        container_guid: row.container_guid,
        container_id: row.container_id,
        started_at: row.started_at,
        expect_stop_at: row.expect_stop_at,
        ip: row.ip,
        port: row.port,
        is_proxy: row.is_proxy,
    })
}

/// `GET /api/admin/instances` — paginated list of managed containers with their
/// concrete team or non-team ownership scope and challenge.
pub async fn instances(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<ListQuery>,
) -> AppResult<ArrayResponse<ContainerInstanceModel>> {
    let count = q.count.clamp(0, 500) as i64;
    let skip = i64::try_from(q.skip).unwrap_or(i64::MAX);
    let total = sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "Containers""#)
        .fetch_one(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let rows = sqlx::query_as::<_, ContainerInstanceRow>(INSTANCE_PAGE_SQL)
        .bind(count)
        .bind(skip)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let data = rows
        .into_iter()
        .map(project_instance)
        .collect::<AppResult<Vec<_>>>()?;

    Ok(ArrayResponse::new(data, total))
}

/// `DELETE /api/admin/instances/{id}` — forcibly destroy a container.
pub async fn destroy_instance(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<Uuid>,
) -> AppResult<MessageResponse> {
    let c = container::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Container instance not found"))?;

    crate::controllers::game::destroy_managed_container_row(&st, &c, false).await?;
    Ok(MessageResponse::ok(""))
}

/// RSCTF `ContainerStatsModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerStatsModel {
    pub cpu_percent: f64,
    pub memory_used_bytes: i64,
    pub memory_limit_bytes: i64,
    pub net_rx_bytes: i64,
    pub net_tx_bytes: i64,
    #[serde(with = "crate::utils::datetime::millis")]
    pub sampled_at: DateTime<Utc>,
}

/// `GET /api/admin/instances/{id}/stats` — point-in-time container stats.
///
/// Mirrors RSCTF `AdminController.GetInstanceStats`: look up the container row by
/// its database GUID, then sample the live runtime via `st.containers.query`,
/// which reads the Docker stats API and returns a `ContainerStatus` with
/// `memory_bytes` / `cpu_usage` populated. The coarse `ContainerStatus` sample
/// carries CPU (as a fraction of one core) and memory (bytes); it does not expose
/// a memory limit or per-interface network counters, so those DTO fields stay `0`
/// (matching the "stats the backend can provide" contract). `cpu_usage` is scaled
/// ×100 to the `cpuPercent` (0–100 × cores) the client renders.
///
/// When the runtime can't provide a sample — no Docker backend configured, the
/// daemon is unreachable, or the container is already gone — `query` errors; we
/// degrade to a 404 with a null payload, exactly like RSCTF returns when
/// `GetStatsAsync` yields `null`.
pub async fn instance_stats(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<Uuid>,
) -> AppResult<RequestResponse<ContainerStatsModel>> {
    let c = container::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Container instance not found"))?;

    // Sample the live runtime. A backend error (Docker unreachable / no backend /
    // container gone) degrades to a 404 "stats unavailable" rather than a 500,
    // so the admin UI just shows the row without a stats overlay.
    let status = st
        .containers
        .query(&c.container_id)
        .await
        .map_err(|_| AppError::not_found("Stats unavailable for this container."))?;

    Ok(RequestResponse::ok(ContainerStatsModel {
        cpu_percent: status.cpu_usage.map(|v| v * 100.0).unwrap_or(0.0),
        memory_used_bytes: status.memory_bytes.map(|m| m as i64).unwrap_or(0),
        // The coarse ContainerStatus sample carries no memory limit or network
        // counters; leave them zero until the backend surfaces them.
        memory_limit_bytes: 0,
        net_rx_bytes: 0,
        net_tx_bytes: 0,
        sampled_at: Utc::now(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    #[test]
    fn ownership_projection_rejects_unknown_database_values() {
        assert_eq!(owner_kind("Shared").unwrap(), ContainerOwnerKind::Shared);
        assert!(owner_kind("LegacyMystery").is_err());
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn instance_projection_resolves_every_ownership_shape() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test database");
        let schema = format!("admin_instances_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .expect("create isolated schema");
        let options = PgConnectOptions::from_str(&database_url)
            .expect("parse test database URL")
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .expect("connect isolated pool");

        sqlx::raw_sql(
            r#"
            CREATE TABLE "Containers" (
              id UUID PRIMARY KEY,
              image TEXT NOT NULL,
              container_id TEXT NOT NULL,
              started_at TIMESTAMPTZ NOT NULL,
              expect_stop_at TIMESTAMPTZ NOT NULL,
              is_proxy BOOLEAN NOT NULL,
              ip TEXT NOT NULL,
              port INTEGER NOT NULL,
              public_ip TEXT,
              public_port INTEGER,
              game_instance_id INTEGER,
              exercise_instance_id INTEGER,
              ad_team_service_id INTEGER
            );
            CREATE TABLE "GameInstances" (
              id INTEGER PRIMARY KEY,
              challenge_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL
            );
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY,
              title TEXT NOT NULL,
              category SMALLINT NOT NULL,
              shared_container_id UUID,
              test_container_id UUID
            );
            CREATE TABLE "Participations" (
              id INTEGER PRIMARY KEY,
              team_id INTEGER NOT NULL
            );
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY,
              name TEXT NOT NULL,
              avatar_hash TEXT
            );
            CREATE TABLE "AdTeamServices" (
              id INTEGER PRIMARY KEY,
              participation_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL,
              container_id TEXT
            );
            CREATE TABLE "ExerciseInstances" (
              id INTEGER PRIMARY KEY,
              exercise_id INTEGER NOT NULL,
              user_id UUID NOT NULL,
              container_id UUID
            );
            CREATE TABLE "ExerciseChallenges" (
              id INTEGER PRIMARY KEY,
              title TEXT NOT NULL,
              category SMALLINT NOT NULL
            );
            CREATE TABLE "AspNetUsers" (
              id UUID PRIMARY KEY,
              user_name TEXT,
              real_name TEXT NOT NULL
            );

            INSERT INTO "Teams" VALUES (7, 'red', 'red-avatar');
            INSERT INTO "Participations" VALUES (11, 7);
            INSERT INTO "GameChallenges"
                (id, title, category, shared_container_id, test_container_id)
            VALUES
                (20, 'per-team', 3, NULL, NULL),
                (21, 'the-hill', 0, '00000000-0000-0000-0000-000000000002', NULL),
                (22, 'admin-test', 0, NULL, '00000000-0000-0000-0000-000000000003');
            INSERT INTO "GameInstances" VALUES (30, 20, 11);
            INSERT INTO "AspNetUsers"
            VALUES ('00000000-0000-0000-0000-000000000099', 'alice', 'Alice');
            INSERT INTO "ExerciseChallenges" VALUES (40, 'practice-web', 3);
            INSERT INTO "ExerciseInstances"
            VALUES (
                41,
                40,
                '00000000-0000-0000-0000-000000000099',
                '00000000-0000-0000-0000-000000000004'
            );
            INSERT INTO "Containers"
                (id, image, container_id, started_at, expect_stop_at, is_proxy,
                 ip, port, public_ip, public_port, game_instance_id,
                 exercise_instance_id, ad_team_service_id)
            VALUES
                ('00000000-0000-0000-0000-000000000001', 'team-image', 'runtime-1',
                 now(), now() + interval '1 hour', TRUE, '10.0.0.1', 8080,
                 NULL, NULL, 30, NULL, NULL),
                ('00000000-0000-0000-0000-000000000002', 'hill-image', 'runtime-2',
                 now(), now() + interval '1 hour', FALSE, '10.0.0.2', 8080,
                 NULL, NULL, NULL, NULL, NULL),
                ('00000000-0000-0000-0000-000000000003', 'test-image', 'runtime-3',
                 now(), now() + interval '1 hour', FALSE, '10.0.0.3', 8080,
                 NULL, NULL, NULL, NULL, NULL),
                ('00000000-0000-0000-0000-000000000004', 'exercise-image', 'runtime-4',
                 now(), now() + interval '1 hour', TRUE, '10.0.0.4', 8080,
                 '203.0.113.4', 443, NULL, 41, NULL),
                ('00000000-0000-0000-0000-000000000005', 'orphan-image', 'runtime-5',
                 now(), now() + interval '1 hour', FALSE, '10.0.0.5', 8080,
                 NULL, NULL, NULL, NULL, NULL);
            "#,
        )
        .execute(&pool)
        .await
        .expect("seed ownership shapes");

        let rows = sqlx::query_as::<_, ContainerInstanceRow>(INSTANCE_PAGE_SQL)
            .bind(100_i64)
            .bind(0_i64)
            .fetch_all(&pool)
            .await
            .expect("project instances");
        let models = rows
            .into_iter()
            .map(project_instance)
            .collect::<AppResult<Vec<_>>>()
            .expect("decode projections");

        let team = &models[0];
        assert_eq!(team.owner_kind, ContainerOwnerKind::Team);
        assert_eq!(
            team.team.as_ref().map(|value| value.name.as_str()),
            Some("red")
        );
        assert_eq!(
            team.challenge.as_ref().map(|value| value.title.as_str()),
            Some("per-team")
        );
        assert!(team.is_proxy);

        let shared = &models[1];
        assert_eq!(shared.owner_kind, ContainerOwnerKind::Shared);
        assert!(shared.team.is_none());
        assert_eq!(
            shared.challenge.as_ref().map(|value| value.title.as_str()),
            Some("the-hill")
        );
        assert!(!shared.is_proxy);

        assert_eq!(models[2].owner_kind, ContainerOwnerKind::AdminTest);
        assert_eq!(models[3].owner_kind, ContainerOwnerKind::Exercise);
        assert_eq!(models[3].owner_name.as_deref(), Some("alice"));
        assert_eq!(models[3].ip, "203.0.113.4");
        assert_eq!(models[3].port, 443);
        assert_eq!(models[4].owner_kind, ContainerOwnerKind::Unassigned);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .expect("drop isolated schema");
        admin.close().await;
    }
}

// ─── Files ─────────────────────────────────────────────────────────────────────

/// RSCTF `LocalFile`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalFileModel {
    pub hash: String,
    pub name: String,
}
