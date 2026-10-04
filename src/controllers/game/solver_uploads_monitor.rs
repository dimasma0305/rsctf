//! Monitor review of solver uploads: every team's versions per challenge with
//! solve-relative timing, and a download that is always an inert attachment.
//! Uploads stay reviewable after the event switch is turned off.

use axum::extract::Query;
use sea_orm::ActiveEnum;

use super::ai_chats::private_json;
use super::solver_uploads::{SolverUploadVersion, VersionRow};
use super::*;

const MAX_MONITOR_PAGE: i64 = 100;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SolverUploadMonitorQuery {
    pub count: Option<i64>,
    pub skip: Option<i64>,
    pub challenge_id: Option<i32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SolverUploadRecord {
    pub participation_id: i32,
    pub team_id: i32,
    pub team_name: String,
    pub challenge_id: i32,
    pub challenge_title: String,
    pub category: ChallengeCategory,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub solved_at: Option<DateTime<Utc>>,
    /// Newest first.
    pub versions: Vec<SolverUploadVersion>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SolverUploadPage {
    pub total: i64,
    pub items: Vec<SolverUploadRecord>,
}

#[derive(sqlx::FromRow)]
struct GroupRow {
    participation_id: i32,
    team_id: i32,
    team_name: String,
    challenge_id: i32,
    challenge_title: String,
    category: i16,
    solved_at: Option<DateTime<Utc>>,
}

#[derive(sqlx::FromRow)]
struct GroupVersionRow {
    participation_id: i32,
    challenge_id: i32,
    #[sqlx(flatten)]
    version: VersionRow,
}

/// `GET /api/game/{id}/solver-uploads` — uploads grouped per team and
/// challenge, most recently uploaded first. Metadata only.
pub async fn list_solver_uploads(
    State(st): State<SharedState>,
    MonitorUser(_user): MonitorUser,
    Path(id): Path<i32>,
    Query(query): Query<SolverUploadMonitorQuery>,
) -> AppResult<Response> {
    load_game(&st, id).await?;
    let count = query.count.unwrap_or(50).clamp(1, MAX_MONITOR_PAGE);
    let skip = query.skip.unwrap_or(0).max(0);
    let total: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM (
               SELECT DISTINCT participation_id, challenge_id FROM "SolverUploads"
                WHERE game_id = $1 AND ($2::integer IS NULL OR challenge_id = $2)
           ) groups"#,
    )
    .bind(id)
    .bind(query.challenge_id)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let groups = sqlx::query_as::<_, GroupRow>(
        r#"WITH groups AS (
               SELECT participation_id, challenge_id, MAX(uploaded_at) AS latest,
                      MAX(solved_at) AS solved_at
                 FROM "SolverUploads"
                WHERE game_id = $1 AND ($2::integer IS NULL OR challenge_id = $2)
                GROUP BY participation_id, challenge_id
           )
           SELECT groups.participation_id, participation.team_id, team.name AS team_name,
                  groups.challenge_id, challenge.title AS challenge_title, challenge.category,
                  groups.solved_at
             FROM groups
             JOIN "Participations" participation ON participation.id = groups.participation_id
             JOIN "Teams" team ON team.id = participation.team_id
             JOIN "GameChallenges" challenge ON challenge.id = groups.challenge_id
            ORDER BY groups.latest DESC, groups.participation_id, groups.challenge_id
            LIMIT $3 OFFSET $4"#,
    )
    .bind(id)
    .bind(query.challenge_id)
    .bind(count)
    .bind(skip)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let participations: Vec<i32> = groups.iter().map(|row| row.participation_id).collect();
    let challenges: Vec<i32> = groups.iter().map(|row| row.challenge_id).collect();
    let versions = sqlx::query_as::<_, GroupVersionRow>(
        r#"SELECT upload.participation_id, upload.challenge_id, upload.id, upload.version,
                  upload.file_name, upload.size_bytes, upload.sha256,
                  account.user_name AS uploaded_by, upload.seconds_since_solve,
                  upload.uploaded_at
             FROM "SolverUploads" upload
             JOIN unnest($2::integer[], $3::integer[]) AS wanted(participation_id, challenge_id)
               ON wanted.participation_id = upload.participation_id
              AND wanted.challenge_id = upload.challenge_id
             LEFT JOIN "AspNetUsers" account ON account.id = upload.uploaded_by
            WHERE upload.game_id = $1
            ORDER BY upload.version DESC"#,
    )
    .bind(id)
    .bind(&participations)
    .bind(&challenges)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let mut by_group: HashMap<(i32, i32), Vec<SolverUploadVersion>> = HashMap::new();
    for row in versions {
        by_group
            .entry((row.participation_id, row.challenge_id))
            .or_default()
            .push(row.version.into());
    }
    let items = groups
        .into_iter()
        .map(|row| {
            Ok(SolverUploadRecord {
                category: ChallengeCategory::try_from_value(&row.category)
                    .map_err(|error| AppError::internal(error.to_string()))?,
                versions: by_group
                    .remove(&(row.participation_id, row.challenge_id))
                    .unwrap_or_default(),
                participation_id: row.participation_id,
                team_id: row.team_id,
                team_name: row.team_name,
                challenge_id: row.challenge_id,
                challenge_title: row.challenge_title,
                solved_at: row.solved_at,
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    Ok(private_json(SolverUploadPage { total, items }))
}

/// Headers that keep an uploaded solver inert in every browser: a download,
/// never rendered, sniffed, cached, or given script privileges.
pub(super) fn download_headers(file_name: &str) -> [(header::HeaderName, String); 5] {
    [
        (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
        (
            header::CONTENT_DISPOSITION,
            crate::utils::content_disposition::attachment(file_name),
        ),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
        (header::CACHE_CONTROL, "private, no-store".to_owned()),
        (header::CONTENT_SECURITY_POLICY, "sandbox".to_owned()),
    ]
}

/// `GET /api/game/{id}/solver-uploads/{uploadId}/file` — download one solver
/// version as an attachment. Monitors only; players never read files back.
pub async fn download_solver_upload(
    State(st): State<SharedState>,
    MonitorUser(_user): MonitorUser,
    Path((id, upload_id)): Path<(i32, i64)>,
) -> AppResult<Response> {
    let row: Option<(String, Vec<u8>)> = sqlx::query_as(
        r#"SELECT file_name, content FROM "SolverUploads" WHERE id = $1 AND game_id = $2"#,
    )
    .bind(upload_id)
    .bind(id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let (file_name, content) = row.ok_or_else(|| AppError::not_found("Solver upload not found"))?;
    Ok((download_headers(&file_name), content).into_response())
}
