//! Challenge build history / image pipeline endpoints.

use super::*;

/// RSCTF `PruneResultModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PruneResultModel {
    pub removed: i32,
    pub messages: Vec<String>,
}

// ─── Challenge build history / pipeline ─────────────────────────────────────
//
// Backed by the `BuildRecords` table (`build_record`). `run_challenge_build`
// in `edit.rs` writes one row per build attempt, so these endpoints surface the
// real history + live in-progress strip the admin Builds page polls.

/// RSCTF `ChallengeBuildAuditModel` — one row of the Builds history table.
/// `durationMs` / `errorMessage` are derived from the persisted timestamps and
/// log tail; every other field maps straight off the `BuildRecords` row.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeBuildAuditModel {
    pub id: i32,
    pub challenge_id: i32,
    pub game_id: i32,
    pub challenge_title: String,
    #[serde(with = "crate::utils::datetime::millis")]
    pub enqueued_at_utc: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub started_at_utc: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub finished_at_utc: Option<DateTime<Utc>>,
    /// BuildTrigger: "Import" | "Manual" | "AutoRetry" | "Bulk".
    pub trigger: String,
    /// ChallengeBuildKind: "Challenge" | "Checker".
    pub kind: String,
    pub attempt: i32,
    pub status: ChallengeBuildStatus,
    pub digest: Option<String>,
    pub image_ref: Option<String>,
    pub log_tail: Option<String>,
    pub error_message: Option<String>,
    pub duration_ms: i64,
}

impl From<build_record::Model> for ChallengeBuildAuditModel {
    fn from(b: build_record::Model) -> Self {
        // Wall-clock build time, when both endpoints are recorded.
        let duration_ms = match (b.started_at_utc, b.finished_at_utc) {
            (Some(s), Some(f)) => (f - s).num_milliseconds().max(0),
            _ => 0,
        };
        // On a failed build, surface the last meaningful log line as the error;
        // the full log stays in `log_tail` (shown in the log modal).
        let error_message = matches!(
            b.status,
            ChallengeBuildStatus::Failed | ChallengeBuildStatus::MissingDockerfile
        )
        .then(|| {
            b.log_tail
                .as_deref()
                .and_then(|log| log.lines().rev().map(str::trim).find(|l| !l.is_empty()))
                .map(|l| l.chars().take(300).collect::<String>())
        })
        .flatten();

        Self {
            id: b.id,
            challenge_id: b.challenge_id,
            game_id: b.game_id,
            challenge_title: b.challenge_title,
            enqueued_at_utc: b.enqueued_at_utc,
            started_at_utc: b.started_at_utc,
            finished_at_utc: b.finished_at_utc,
            trigger: b.trigger,
            kind: b.kind,
            attempt: b.attempt,
            status: b.status,
            digest: b.digest,
            image_ref: b.image_ref,
            log_tail: b.log_tail,
            error_message,
            duration_ms,
        }
    }
}

/// RSCTF `ChallengeBuildInProgressModel` — one row of the live in-progress strip.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeBuildInProgressModel {
    pub audit_id: i32,
    pub challenge_id: i32,
    pub game_id: i32,
    pub slug: String,
    pub attempt: i32,
    pub trigger: String,
    pub kind: String,
    #[serde(with = "crate::utils::datetime::millis")]
    pub started_at_utc: DateTime<Utc>,
}

impl From<build_record::Model> for ChallengeBuildInProgressModel {
    fn from(b: build_record::Model) -> Self {
        Self {
            audit_id: b.id,
            challenge_id: b.challenge_id,
            game_id: b.game_id,
            slug: b.challenge_title,
            attempt: b.attempt,
            trigger: b.trigger,
            kind: b.kind,
            // A queued row may not have started yet — fall back to the enqueue time.
            started_at_utc: b.started_at_utc.unwrap_or(b.enqueued_at_utc),
        }
    }
}

/// RSCTF `BuildImageModel` — one `rsctf/*` image on the local docker daemon.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildImageModel {
    pub id: String,
    pub tags: Vec<String>,
    pub size_bytes: i64,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub created_utc: Option<DateTime<Utc>>,
    pub referenced: bool,
    pub referenced_by: Vec<String>,
    pub is_checker: bool,
}

/// `GET /api/admin/builds` query (`?count=&skip=&status=&gameId=`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildsListQuery {
    #[serde(default = "default_count")]
    pub count: u64,
    #[serde(default)]
    pub skip: u64,
    #[serde(default)]
    pub status: Option<ChallengeBuildStatus>,
    #[serde(default)]
    pub game_id: Option<i32>,
}

/// `DELETE /api/admin/builds/images` query (`?tag=&force=`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteImageQuery {
    pub tag: String,
    #[serde(default)]
    pub force: bool,
}

/// `GET /api/admin/builds` — paginated build history, newest first. Optional
/// `status` / `gameId` filters mirror the generated client; the page ships the
/// raw `ChallengeBuildAuditModel[]` (the UI filters/paginates in-memory).
pub async fn list_builds(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<BuildsListQuery>,
) -> AppResult<RequestResponse<Vec<ChallengeBuildAuditModel>>> {
    let count = q.count.clamp(1, 500);
    let mut base = build_record::Entity::find();
    if let Some(status) = q.status {
        base = base.filter(build_record::Column::Status.eq(status));
    }
    if let Some(game_id) = q.game_id {
        base = base.filter(build_record::Column::GameId.eq(game_id));
    }
    let rows = base
        .order_by_desc(build_record::Column::EnqueuedAtUtc)
        .order_by_desc(build_record::Column::Id)
        .offset(q.skip)
        .limit(count)
        .all(&st.db)
        .await?;
    Ok(RequestResponse::ok(
        rows.into_iter().map(Into::into).collect(),
    ))
}

/// `GET /api/admin/builds/inprogress` — builds still pending/running
/// (`Queued` / `Building`), newest first.
pub async fn builds_in_progress(
    State(st): State<SharedState>,
    _admin: AdminUser,
) -> AppResult<RequestResponse<Vec<ChallengeBuildInProgressModel>>> {
    let rows = build_record::Entity::find()
        .filter(
            Condition::any()
                .add(build_record::Column::Status.eq(ChallengeBuildStatus::Building))
                .add(build_record::Column::Status.eq(ChallengeBuildStatus::Queued)),
        )
        .order_by_desc(build_record::Column::EnqueuedAtUtc)
        .order_by_desc(build_record::Column::Id)
        .all(&st.db)
        .await?;
    Ok(RequestResponse::ok(
        rows.into_iter().map(Into::into).collect(),
    ))
}

/// `DELETE /api/admin/builds/{auditId}` — drop a single audit row.
pub async fn delete_build(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(audit_id): Path<i32>,
) -> AppResult<MessageResponse> {
    build_record::Entity::delete_by_id(audit_id)
        .exec(&st.db)
        .await?;
    Ok(MessageResponse::ok(""))
}

/// `POST /api/admin/builds/prunefailed` — drop every `Failed` audit row.
pub async fn prune_failed_builds(
    State(st): State<SharedState>,
    _admin: AdminUser,
) -> AppResult<RequestResponse<PruneResultModel>> {
    let res = build_record::Entity::delete_many()
        .filter(build_record::Column::Status.eq(ChallengeBuildStatus::Failed))
        .exec(&st.db)
        .await?;
    Ok(RequestResponse::ok(PruneResultModel {
        removed: res.rows_affected as i32,
        messages: Vec::new(),
    }))
}

/// `POST /api/admin/builds/bulkdelete` — drop an explicit list of audit-row ids.
pub async fn bulk_delete_builds(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Json(ids): Json<Vec<i32>>,
) -> AppResult<RequestResponse<PruneResultModel>> {
    if ids.is_empty() {
        return Ok(RequestResponse::ok(PruneResultModel {
            removed: 0,
            messages: Vec::new(),
        }));
    }
    let res = build_record::Entity::delete_many()
        .filter(build_record::Column::Id.is_in(ids))
        .exec(&st.db)
        .await?;
    Ok(RequestResponse::ok(PruneResultModel {
        removed: res.rows_affected as i32,
        messages: Vec::new(),
    }))
}

/// `POST /api/admin/builds/{auditId}/reenqueue` — re-run the build for the
/// challenge owning this audit row (same seam as an interactive rebuild, tagged
/// `AutoRetry`) and echo back the freshly-recorded audit row. Falls back to
/// recording a fresh `Queued` row from the old snapshot when the challenge is
/// gone (or the seam's own audit write failed).
pub async fn reenqueue_build(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(audit_id): Path<i32>,
) -> AppResult<RequestResponse<ChallengeBuildAuditModel>> {
    let record = build_record::Entity::find_by_id(audit_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Build record not found"))?;

    let attempt = record.attempt + 1;

    // The challenge may have been deleted since the original build ran.
    let new_record = match game_challenge::Entity::find_by_id(record.challenge_id)
        .one(&st.db)
        .await?
    {
        Some(challenge) => {
            crate::controllers::edit::admin_reenqueue_build(&st, &challenge, attempt).await
        }
        None => None,
    };

    let model = match new_record {
        Some(m) => m,
        None => {
            build_record::ActiveModel {
                challenge_id: Set(record.challenge_id),
                game_id: Set(record.game_id),
                challenge_title: Set(record.challenge_title.clone()),
                enqueued_at_utc: Set(Utc::now()),
                started_at_utc: Set(None),
                finished_at_utc: Set(None),
                trigger: Set("AutoRetry".to_string()),
                kind: Set(record.kind.clone()),
                attempt: Set(attempt),
                status: Set(ChallengeBuildStatus::Queued),
                digest: Set(None),
                image_ref: Set(None),
                log_tail: Set(Some("Re-enqueued from the admin Builds page.".to_string())),
                ..Default::default()
            }
            .insert(&st.db)
            .await?
        }
    };

    Ok(RequestResponse::ok(model.into()))
}

/// Connect to the local docker daemon and confirm a short-timeout ping. Returns
/// `None` when the daemon is absent/unreachable so the image endpoints degrade
/// to an empty inventory instead of erroring.
async fn reachable_docker() -> Option<Docker> {
    let docker = Docker::connect_with_local_defaults().ok()?;
    match tokio::time::timeout(std::time::Duration::from_secs(2), docker.ping()).await {
        Ok(Ok(_)) => Some(docker),
        _ => None,
    }
}

/// Query the daemon for `rsctf/*` images and cross-reference them against
/// the challenges still pointing at them. Isolated so any bollard/DB error maps
/// to an empty inventory at the call site (never a fabricated list).
async fn collect_build_images(
    st: &SharedState,
    docker: &Docker,
) -> Result<Vec<BuildImageModel>, ()> {
    let images = docker
        .list_images(Some(ListImagesOptions::<String> {
            all: false,
            ..Default::default()
        }))
        .await
        .map_err(|_| ())?;

    // Map every image ref a challenge still references (service + A&D checker)
    // to the owning challenge title, for the "referenced by" column.
    let challenges = game_challenge::Entity::find()
        .all(&st.db)
        .await
        .map_err(|_| ())?;
    let mut ref_titles: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for c in &challenges {
        for img in [c.container_image.as_deref(), c.ad_checker_image.as_deref()] {
            if let Some(img) = img.filter(|s| !s.is_empty()) {
                ref_titles
                    .entry(img.to_string())
                    .or_default()
                    .push(c.title.clone());
            }
        }
    }

    let mut out = Vec::new();
    for img in images {
        let tags: Vec<String> = img
            .repo_tags
            .into_iter()
            .filter(|t| t.starts_with("rsctf/"))
            .collect();
        if tags.is_empty() {
            continue;
        }
        let referenced_by: Vec<String> = tags
            .iter()
            .filter_map(|t| ref_titles.get(t))
            .flatten()
            .cloned()
            .collect();
        let is_checker = tags.iter().any(|t| t.contains("checker"));
        let created_utc = DateTime::<Utc>::from_timestamp(img.created, 0);
        out.push(BuildImageModel {
            id: img.id,
            referenced: !referenced_by.is_empty(),
            referenced_by,
            is_checker,
            size_bytes: img.size,
            created_utc,
            tags,
        });
    }
    Ok(out)
}

/// `GET /api/admin/builds/images` — `rsctf/*` images on the local docker
/// daemon. Docker-gated: an absent/unreachable daemon yields an empty inventory
/// (never a 5xx, never a fabricated list).
pub async fn build_images(
    State(st): State<SharedState>,
    _admin: AdminUser,
) -> RequestResponse<Vec<BuildImageModel>> {
    let images = match reachable_docker().await {
        Some(docker) => collect_build_images(&st, &docker).await.unwrap_or_default(),
        None => Vec::new(),
    };
    RequestResponse::ok(images)
}

/// `DELETE /api/admin/builds/images?tag=&force=` — remove one image from the
/// local docker daemon by tag. Docker-gated / best-effort.
pub async fn delete_build_image(
    _admin: AdminUser,
    Query(q): Query<DeleteImageQuery>,
) -> RequestResponse<PruneResultModel> {
    let removed = match reachable_docker().await {
        Some(docker) => {
            let opts = RemoveImageOptions {
                force: q.force,
                ..Default::default()
            };
            docker
                .remove_image(&q.tag, Some(opts), None)
                .await
                .map(|items| items.len() as i32)
                .unwrap_or(0)
        }
        None => 0,
    };
    RequestResponse::ok(PruneResultModel {
        removed,
        messages: Vec::new(),
    })
}

/// `POST /api/admin/builds/pruneimages` — GC every `rsctf/*` image no
/// challenge still references. Docker-gated / best-effort.
pub async fn prune_images(
    State(st): State<SharedState>,
    _admin: AdminUser,
) -> RequestResponse<PruneResultModel> {
    let Some(docker) = reachable_docker().await else {
        return RequestResponse::ok(PruneResultModel {
            removed: 0,
            messages: Vec::new(),
        });
    };
    let images = collect_build_images(&st, &docker).await.unwrap_or_default();
    let mut removed = 0i32;
    let mut messages = Vec::new();
    for img in images {
        if img.referenced {
            continue;
        }
        for tag in img.tags {
            let opts = RemoveImageOptions {
                force: false,
                ..Default::default()
            };
            match docker.remove_image(&tag, Some(opts), None).await {
                Ok(items) => removed += items.len() as i32,
                Err(e) => messages.push(format!("{tag}: {e}")),
            }
        }
    }
    RequestResponse::ok(PruneResultModel { removed, messages })
}
