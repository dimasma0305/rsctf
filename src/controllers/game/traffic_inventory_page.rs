//! Bounded, metadata-bearing traffic inventory pages.

use super::*;

const CAPTURE_PAGE_LISTING_CONCURRENCY: usize = 4;
static CAPTURE_PAGE_LISTING_SLOTS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| {
        std::sync::Arc::new(tokio::sync::Semaphore::new(
            CAPTURE_PAGE_LISTING_CONCURRENCY,
        ))
    });

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureInventoryPage<T> {
    pub items: Vec<T>,
    pub total: usize,
    pub next_skip: Option<usize>,
}

pub(super) fn next_capture_skip(skip: usize, scanned: usize, total: usize) -> Option<usize> {
    let next = skip.saturating_add(scanned);
    (scanned > 0 && next < total).then_some(next)
}

pub(super) async fn load_team_traffic_page(
    st: &SharedState,
    challenge_id: i32,
    page: CapturePageQuery,
) -> AppResult<CaptureInventoryPage<Json>> {
    let (skip, count) = page.normalized()?;
    let inventory = crate::services::traffic::inventory::load(capture_root(st)).await?;
    let mut matching = inventory
        .directories
        .iter()
        .filter(|directory| directory.challenge_id == challenge_id && !directory.files.is_empty())
        .collect::<Vec<_>>();
    matching.sort_unstable_by_key(|directory| directory.participation_id);
    let total = matching.len();
    let captures = matching
        .into_iter()
        .skip(skip)
        .take(count)
        .map(|directory| (directory.participation_id, directory.files.len()))
        .collect::<Vec<_>>();
    let scanned = captures.len();
    if captures.is_empty() {
        return Ok(CaptureInventoryPage {
            items: Vec::new(),
            total,
            next_skip: None,
        });
    }

    let participation_ids = captures.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    let teams = sqlx::query_as::<_, CaptureTeamRow>(
        r#"SELECT p.id AS participation_id,
                  p.team_id,
                  t.name,
                  t.avatar_hash
             FROM "Participations" p
             JOIN "Teams" t ON t.id = p.team_id
             JOIN "GameChallenges" challenge
               ON challenge.id = $2 AND challenge.game_id = p.game_id
            WHERE p.id = ANY($1)"#,
    )
    .bind(&participation_ids)
    .bind(challenge_id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .into_iter()
    .map(|row| (row.participation_id, row))
    .collect::<std::collections::HashMap<_, _>>();
    let items = captures
        .into_iter()
        .filter_map(|(participation_id, capture_count)| {
            let team = teams.get(&participation_id)?;
            let avatar = team
                .avatar_hash
                .as_ref()
                .map(|hash| format!("/assets/{hash}/avatar"));
            Some(serde_json::json!({
                "id": participation_id,
                "teamId": team.team_id,
                "name": team.name,
                "division": Json::Null,
                "avatar": avatar,
                "count": capture_count,
            }))
        })
        .collect();
    Ok(CaptureInventoryPage {
        items,
        total,
        next_skip: next_capture_skip(skip, scanned, total),
    })
}

pub(super) async fn load_traffic_files_page(
    st: &SharedState,
    challenge_id: i32,
    participation_id: i32,
    page: CapturePageQuery,
) -> AppResult<CaptureInventoryPage<Json>> {
    let (skip, count) = page.normalized()?;
    let root = capture_root(st);
    let inventory = crate::services::traffic::inventory::load_directory(
        root.clone(),
        challenge_id,
        participation_id,
    )
    .await?;
    let total = inventory.as_ref().map_or(0, |value| value.files.len());
    let names = inventory
        .map(|value| {
            value
                .files
                .into_iter()
                .skip(skip)
                .take(count)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let scanned = names.len();
    let directory = root
        .join(challenge_id.to_string())
        .join(participation_id.to_string());
    let listing_permit = std::sync::Arc::clone(&CAPTURE_PAGE_LISTING_SLOTS)
        .try_acquire_owned()
        .map_err(|_| {
            AppError::retryable_unavailable(
                "Capture file inventory capacity is busy; retry shortly",
                2,
            )
        })?;
    let items = tokio::task::spawn_blocking(move || -> AppResult<Vec<Json>> {
        // Keep the process permit inside the blocking task. If the client
        // disconnects and its join handle is dropped, Tokio detaches the task;
        // releasing the permit in the request future would then defeat the
        // process-wide ceiling.
        let _listing_permit = listing_permit;
        Ok(names
            .into_iter()
            .filter_map(|name| {
                let metadata = std::fs::symlink_metadata(directory.join(&name)).ok()?;
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return None;
                }
                let update_time = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_millis() as i64)
                    .unwrap_or(0);
                Some(serde_json::json!({
                    "fileName": name,
                    "size": metadata.len(),
                    "updateTime": update_time,
                }))
            })
            .collect())
    })
    .await
    .map_err(|error| AppError::internal(format!("capture listing task failed: {error}")))??;
    Ok(CaptureInventoryPage {
        items,
        total,
        next_skip: next_capture_skip(skip, scanned, total),
    })
}

#[cfg(test)]
pub(super) fn capture_page_listing_concurrency() -> usize {
    CAPTURE_PAGE_LISTING_CONCURRENCY
}

pub async fn team_traffic_page(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(challenge_id): Path<i32>,
    Query(page): Query<CapturePageQuery>,
) -> AppResult<RequestResponse<CaptureInventoryPage<Json>>> {
    Ok(RequestResponse::ok(
        load_team_traffic_page(&st, challenge_id, page).await?,
    ))
}

pub async fn traffic_files_page(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path((challenge_id, participation_id)): Path<(i32, i32)>,
    Query(page): Query<CapturePageQuery>,
) -> AppResult<RequestResponse<CaptureInventoryPage<Json>>> {
    Ok(RequestResponse::ok(
        load_traffic_files_page(&st, challenge_id, participation_id, page).await?,
    ))
}
