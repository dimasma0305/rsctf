//! Bounded flag-editor reads and legacy policy-violation reporting.

use super::*;

const DEFAULT_PAGE_SIZE: i64 = 100;
const MAX_PAGE_SIZE: i64 = 100;
const MAX_PAGE_OFFSET: i64 = 512;
const MAX_REPORTED_VIOLATIONS: i64 = 20;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlagPageQuery {
    #[serde(default)]
    offset: i64,
    #[serde(default = "default_page_size")]
    limit: i64,
}

fn default_page_size() -> i64 {
    DEFAULT_PAGE_SIZE
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlagPolicyViolationModel {
    pub flag_context_id: Option<i32>,
    pub violation_type: String,
    pub observed_bytes: i64,
    pub detected_at_utc: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlagPageModel {
    pub items: Vec<FlagInfoModel>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
    pub violation_count: i64,
    pub violations: Vec<FlagPolicyViolationModel>,
}

#[derive(sqlx::FromRow)]
struct FlagRow {
    id: i32,
    flag: String,
    attachment_id: Option<i32>,
    file_type: Option<i16>,
    remote_url: Option<String>,
    file_hash: Option<String>,
    file_name: Option<String>,
    file_size: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct ViolationRow {
    flag_context_id: Option<i32>,
    violation_type: String,
    observed_bytes: i64,
    detected_at_utc: DateTime<Utc>,
}

fn normalize_page(query: FlagPageQuery) -> AppResult<(i64, i64)> {
    if query.offset < 0 || query.offset > MAX_PAGE_OFFSET {
        return Err(AppError::bad_request("Flag page offset is out of range"));
    }
    if query.limit < 1 || query.limit > MAX_PAGE_SIZE {
        return Err(AppError::bad_request(format!(
            "Flag page limit must be between 1 and {MAX_PAGE_SIZE}"
        )));
    }
    Ok((query.offset, query.limit))
}

fn project_flag(row: FlagRow) -> AppResult<FlagInfoModel> {
    let attachment = match (row.attachment_id, row.file_type) {
        (Some(id), Some(raw_type)) => {
            let file_type = match raw_type {
                value if value == FileType::None as i16 => FileType::None,
                value if value == FileType::Local as i16 => FileType::Local,
                value if value == FileType::Remote as i16 => FileType::Remote,
                _ => return Err(AppError::internal("Invalid attachment type")),
            };
            let (url, file_size) = match file_type {
                FileType::None => (None, None),
                FileType::Remote => (row.remote_url, None),
                FileType::Local => (
                    row.file_hash
                        .zip(row.file_name)
                        .map(|(hash, name)| format!("/assets/{hash}/{name}")),
                    row.file_size,
                ),
            };
            Some(AttachmentInfoModel {
                id,
                file_type,
                url,
                file_size,
            })
        }
        _ => None,
    };
    Ok(FlagInfoModel {
        id: row.id,
        flag: row.flag,
        attachment,
    })
}

async fn load_flag_page(
    st: &SharedState,
    challenge_id: i32,
    offset: i64,
    limit: i64,
) -> AppResult<FlagPageModel> {
    let total = sqlx::query_scalar::<_, i64>(
        r#"SELECT COUNT(*)::BIGINT FROM "FlagContexts"
            WHERE challenge_id = $1 AND is_occupied = FALSE"#,
    )
    .bind(challenge_id)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if total > MAX_PAGE_OFFSET {
        return Err(AppError::payload_too_large(
            "This challenge exceeds the editable flag limit; remove legacy flags first",
        ));
    }
    let rows = sqlx::query_as::<_, FlagRow>(
        r#"SELECT context.id, context.flag,
                  attachment.id AS attachment_id,
                  attachment."Type" AS file_type,
                  attachment.remote_url,
                  file.hash AS file_hash,
                  file.name AS file_name,
                  file.file_size
             FROM "FlagContexts" context
             LEFT JOIN "Attachments" attachment ON attachment.id = context.attachment_id
             LEFT JOIN "Files" file ON file.id = attachment.local_file_id
            WHERE context.challenge_id = $1 AND context.is_occupied = FALSE
            ORDER BY context.id
            LIMIT $2 OFFSET $3"#,
    )
    .bind(challenge_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let violation_count = sqlx::query_scalar::<_, i64>(
        r#"SELECT COUNT(*)::BIGINT FROM "FlagPolicyViolations"
            WHERE challenge_id = $1"#,
    )
    .bind(challenge_id)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let violations = sqlx::query_as::<_, ViolationRow>(
        r#"SELECT flag_context_id, violation_type, observed_bytes, detected_at_utc
             FROM "FlagPolicyViolations"
            WHERE challenge_id = $1
            ORDER BY detected_at_utc DESC, id DESC
            LIMIT $2"#,
    )
    .bind(challenge_id)
    .bind(MAX_REPORTED_VIOLATIONS)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(FlagPageModel {
        items: rows
            .into_iter()
            .map(project_flag)
            .collect::<AppResult<Vec<_>>>()?,
        total,
        offset,
        limit,
        violation_count,
        violations: violations
            .into_iter()
            .map(|row| FlagPolicyViolationModel {
                flag_context_id: row.flag_context_id,
                violation_type: row.violation_type,
                observed_bytes: row.observed_bytes,
                detected_at_utc: row.detected_at_utc.timestamp_millis(),
            })
            .collect(),
    })
}

pub(crate) async fn load_flags(
    st: &SharedState,
    challenge_id: i32,
) -> AppResult<Vec<FlagInfoModel>> {
    Ok(load_flag_page(st, challenge_id, 0, DEFAULT_PAGE_SIZE)
        .await?
        .items)
}

/// `GET /api/edit/games/{id}/challenges/{cId}/flags`.
pub async fn get_flags(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
    axum::extract::Query(query): axum::extract::Query<FlagPageQuery>,
) -> AppResult<RequestResponse<FlagPageModel>> {
    manager_or_admin(&st, &user, game_id).await?;
    load_challenge(&st, game_id, challenge_id).await?;
    let (offset, limit) = normalize_page(query)?;
    Ok(RequestResponse::ok(
        load_flag_page(&st, challenge_id, offset, limit).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_bounds_reject_amplifying_reads() {
        assert!(normalize_page(FlagPageQuery {
            offset: 0,
            limit: 100
        })
        .is_ok());
        assert!(normalize_page(FlagPageQuery {
            offset: 0,
            limit: 101
        })
        .is_err());
        assert!(normalize_page(FlagPageQuery {
            offset: 513,
            limit: 1
        })
        .is_err());
    }
}
