//! Ported from RSCTF `Controllers/EditController.cs` — the organizer/admin
//! data-modification API. Route prefix `/api/edit`; routes are gated on platform
//! admin (`AdminUser`) except the manager-visible reads (e.g. get_games), which
//!
//! Core CRUD (games, challenges, flags, notices, divisions, posts, reviews) is
//! fully implemented against the database. The genuinely-infrastructure
//! endpoints (A&D live console, container test/rebuild, git/zip import/export,
//! audit metadata, admin delegation) return a **valid, well-typed empty**
//! success so the React ClientApp stays functional — never a 4xx. Those are
//! marked with `// TODO`.

use axum::body::Body;
use axum::extract::{Multipart, Path, State};
use axum::http::header;
use axum::response::Response;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use bollard::image::{BuildImageOptions, CreateImageOptions};
use bollard::Docker;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value as JsonValue};
use std::io::{Cursor, Read, Write};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::controllers::game::ContainerInfoModel;
use crate::middlewares::privilege_authentication::{AdminUser, CurrentUser};
use crate::models::data::{
    ad_check_result, ad_flag, ad_round, ad_team_service, attachment, build_record,
    challenge_review, container, division, division_challenge_config, flag_context, game,
    game_challenge, game_instance, game_manager, game_notice, koth_target, local_file,
    participation, post, team, user,
};
use crate::services::container::ContainerSpec;
use crate::utils::codec::sha256_str;
use crate::utils::enums::{
    ChallengeBuildStatus, ChallengeCategory, ChallengeReviewStatus, ChallengeType, ContainerStatus,
    FileType, GamePermission, NetworkMode, NoticeType, ParticipationStatus, ReviewRating, Role,
    ScoreCurve,
};
use crate::utils::error::{AppError, AppResult};
use crate::utils::flag_generator;
use crate::utils::shared::{ArrayResponse, MessageResponse, PageParams, RequestResponse};

const BLOOD_BONUS_DEFAULT: i64 = (50 << 20) + (30 << 10) + 10;

/// Port of RSCTF `BloodBonus.FromValue`: a packed value whose any of the three
/// 10-bit fields ((v>>0)&0x3ff, (v>>10)&0x3ff, (v>>20)&0x3ff) exceeds 1000 is
/// rejected, falling back to the default packed value.
fn blood_bonus_from_value(value: i64) -> i64 {
    const MASK: i64 = 0x3ff;
    const BASE: i64 = 1000;
    if (value & MASK) > BASE || ((value >> 10) & MASK) > BASE || ((value >> 20) & MASK) > BASE {
        BLOOD_BONUS_DEFAULT
    } else {
        value
    }
}

fn epoch() -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(0, 0).expect("unix epoch is a valid timestamp")
}
fn default_true() -> bool {
    true
}
fn default_container_limit() -> i32 {
    3
}
fn default_blood_bonus() -> i64 {
    BLOOD_BONUS_DEFAULT
}

// ============================================================================
//  DTOs
// ============================================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostEditModel {
    pub title: Option<String>,
    pub summary: Option<String>,
    pub content: Option<String>,
    pub tags: Option<Vec<String>>,
    pub is_pinned: Option<bool>,
}

/// RSCTF `PostDetailModel` — the outbound editor view for a post. `time` is a
/// `DateTime` serialized as ISO-8601 (matching the info controller's mapping).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostDetailModel {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub content: String,
    pub is_pinned: bool,
    pub tags: Option<Vec<String>>,
    pub author_avatar: Option<String>,
    pub author_name: Option<String>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub time: DateTime<Utc>,
}

impl PostDetailModel {
    fn from_post(p: &post::Model, author_name: Option<String>) -> Self {
        let tags = p
            .tags
            .as_ref()
            .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok());
        Self {
            id: p.id.clone(),
            title: p.title.clone(),
            summary: p.summary.clone(),
            content: p.content.clone(),
            is_pinned: p.is_pinned,
            tags,
            author_avatar: None,
            author_name,
            time: p.update_time_utc,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeInfoModel {
    #[serde(default)]
    pub title: String,
    #[serde(default = "default_category")]
    pub category: ChallengeCategory,
    #[serde(default = "default_type", rename = "type")]
    pub challenge_type: ChallengeType,
}
fn default_category() -> ChallengeCategory {
    ChallengeCategory::Misc
}
fn default_type() -> ChallengeType {
    ChallengeType::StaticAttachment
}
fn default_score_curve() -> ScoreCurve {
    ScoreCurve::Standard
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeUpdateModel {
    pub title: Option<String>,
    pub content: Option<String>,
    pub flag_template: Option<String>,
    pub category: Option<ChallengeCategory>,
    pub hints: Option<Vec<String>>,
    pub is_enabled: Option<bool>,
    pub file_name: Option<String>,
    #[serde(default, with = "crate::utils::datetime::millis_opt")]
    pub deadline_utc: Option<DateTime<Utc>>,
    pub submission_limit: Option<i32>,
    pub container_image: Option<String>,
    pub memory_limit: Option<i32>,
    #[serde(rename = "cpuCount")]
    pub cpu_count: Option<i32>,
    pub storage_limit: Option<i32>,
    pub expose_port: Option<i32>,
    /// Missing preserves the current aggregate definition; JSON null clears it.
    #[serde(default, deserialize_with = "present_optional")]
    pub workload_spec: Option<Option<rsctf_worker_protocol::WorkloadSpec>>,
    pub original_score: Option<i32>,
    pub min_score_rate: Option<f64>,
    pub difficulty: Option<f64>,
    pub score_curve: Option<ScoreCurve>,
    pub enable_traffic_capture: Option<bool>,
    pub disable_blood_bonus: Option<bool>,
    /// Container network mode (RSCTF `ChallengeUpdateModel.NetworkMode`). Absent/
    /// null keeps the stored value (`NetworkMode = model.NetworkMode ?? NetworkMode`).
    pub network_mode: Option<NetworkMode>,
    /// Whether all teams share a single container (StaticContainer only).
    pub enable_shared_container: Option<bool>,
    // --- Attack & Defense per-challenge knobs ---
    pub ad_checker_image: Option<String>,
    pub ad_allow_egress: Option<bool>,
    pub ad_allow_self_reset: Option<bool>,
    pub ad_ssh_requires_flag: Option<bool>,
    pub ad_self_hosted: Option<bool>,
    pub ad_scoring_weight: Option<f64>,
}

fn present_optional<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlagInfoModel {
    pub id: i32,
    pub flag: String,
    /// `Attachment` corresponding to the flag (RSCTF `FlagInfoModel.Attachment`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment: Option<AttachmentInfoModel>,
}

/// RSCTF `Attachment` response projection (id/type/url/fileSize). `url` and
/// `fileSize` are resolved from the joined `LocalFile` for local attachments,
/// or straight from `remoteUrl` for remote ones.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentInfoModel {
    pub id: i32,
    #[serde(rename = "type")]
    pub file_type: FileType,
    pub url: Option<String>,
    pub file_size: Option<i64>,
}

impl AttachmentInfoModel {
    /// Build the response projection from a persisted attachment + its optional
    /// local file, mirroring `Attachment.Url`/`Attachment.FileSize`.
    fn from_attachment(a: &attachment::Model, file: Option<&local_file::Model>) -> Self {
        let (url, file_size) = match a.file_type {
            FileType::None => (None, None),
            FileType::Remote => (a.remote_url.clone(), None),
            FileType::Local => match file {
                Some(f) => (
                    Some(format!("/assets/{}/{}", f.hash, f.name)),
                    Some(f.file_size),
                ),
                None => (None, None),
            },
        };
        Self {
            id: a.id,
            file_type: a.file_type,
            url,
            file_size,
        }
    }
}

/// Summary row for the challenge list (`ChallengeInfoModel.FromChallenge`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeSummaryModel {
    pub id: i32,
    pub title: String,
    pub category: ChallengeCategory,
    #[serde(rename = "type")]
    pub challenge_type: ChallengeType,
    pub score: i32,
    pub min_score: i32,
    pub original_score: i32,
    pub is_enabled: bool,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub deadline_utc: Option<DateTime<Utc>>,
    pub review_status: ChallengeReviewStatus,
    pub build_status: ChallengeBuildStatus,
    pub has_original_archive: bool,
}

impl ChallengeSummaryModel {
    fn from_challenge(c: &game_challenge::Model) -> Self {
        Self {
            id: c.id,
            title: c.title.clone(),
            category: c.category,
            challenge_type: c.challenge_type,
            score: c.original_score,
            min_score: (c.min_score_rate * c.original_score as f64).floor() as i32,
            original_score: c.original_score,
            is_enabled: c.is_enabled,
            deadline_utc: c.deadline_utc,
            review_status: c.review_status,
            build_status: c.build_status,
            has_original_archive: c.original_archive_blob_path.is_some(),
        }
    }
}

/// Full challenge editor view (`ChallengeEditDetailModel.FromChallenge`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeEditDetailModel {
    pub id: i32,
    pub title: String,
    pub content: String,
    pub category: ChallengeCategory,
    #[serde(rename = "type")]
    pub challenge_type: ChallengeType,
    pub hints: Option<JsonValue>,
    pub flag_template: Option<String>,
    pub is_enabled: bool,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub deadline_utc: Option<DateTime<Utc>>,
    pub submission_limit: i32,
    pub accepted_count: i32,
    pub container_image: Option<String>,
    pub memory_limit: Option<i32>,
    pub storage_limit: Option<i32>,
    #[serde(rename = "cpuCount")]
    pub cpu_count: Option<i32>,
    pub expose_port: Option<i32>,
    pub workload_spec: Option<JsonValue>,
    pub workload_identity: Option<String>,
    pub file_name: Option<String>,
    pub original_score: i32,
    pub min_score_rate: f64,
    pub difficulty: f64,
    pub score_curve: ScoreCurve,
    pub enable_traffic_capture: bool,
    pub enable_shared_container: bool,
    pub disable_blood_bonus: bool,
    pub network_mode: Option<NetworkMode>,
    pub attachment: Option<JsonValue>,
    pub test_container: Option<JsonValue>,
    pub build_status: ChallengeBuildStatus,
    pub last_build_log: Option<String>,
    pub ad_checker_image: Option<String>,
    pub ad_allow_egress: bool,
    pub ad_allow_self_reset: bool,
    pub ad_ssh_requires_flag: bool,
    pub ad_self_hosted: bool,
    pub ad_scoring_weight: f64,
    pub flags: Vec<FlagInfoModel>,
}

/// Load a challenge's linked attachment (+ its local file) as the wire JSON plus
/// the derived display file name, so the edit view shows an uploaded attachment.
async fn load_challenge_attachment(
    st: &SharedState,
    attachment_id: Option<i32>,
) -> (Option<JsonValue>, Option<String>) {
    let Some(aid) = attachment_id else {
        return (None, None);
    };
    let Some(a) = attachment::Entity::find_by_id(aid)
        .one(&st.db)
        .await
        .ok()
        .flatten()
    else {
        return (None, None);
    };
    let file = match a.local_file_id {
        Some(fid) => local_file::Entity::find_by_id(fid)
            .one(&st.db)
            .await
            .ok()
            .flatten(),
        None => None,
    };
    let name = file.as_ref().map(|f| f.name.clone());
    let info = AttachmentInfoModel::from_attachment(&a, file.as_ref());
    (serde_json::to_value(info).ok(), name)
}

impl ChallengeEditDetailModel {
    async fn from_challenge(
        st: &SharedState,
        c: &game_challenge::Model,
        flags: Vec<FlagInfoModel>,
    ) -> AppResult<Self> {
        // Load the linked attachment so the UI shows it after an upload — RSCTF
        // returns the attachment + its derived fileName from this endpoint.
        let (attachment, attach_name) = load_challenge_attachment(st, c.attachment_id).await;
        // Re-surface a live test container so the edit UI keeps showing it across
        // reloads (the client sets `testContainer` on create, then re-reads it
        // here). Was hardcoded None, so a refresh dropped the running container.
        let test_container = match c.test_container_id {
            Some(cid) => container::Entity::find_by_id(cid)
                .one(&st.db)
                .await
                .ok()
                .flatten()
                .and_then(|cont| serde_json::to_value(ContainerInfoModel::from(&cont)).ok()),
            None => None,
        };
        let workload_identity = crate::services::challenge_workloads::from_challenge(c)?
            .map(|spec| crate::services::challenge_workloads::workload_identity(&spec))
            .transpose()?;
        Ok(Self {
            id: c.id,
            title: c.title.clone(),
            content: c.content.clone(),
            category: c.category,
            challenge_type: c.challenge_type,
            hints: c.hints.clone(),
            flag_template: c.flag_template.clone(),
            is_enabled: c.is_enabled,
            deadline_utc: c.deadline_utc,
            submission_limit: c.submission_limit,
            accepted_count: c.accepted_count,
            container_image: c.container_image.clone(),
            memory_limit: c.memory_limit,
            storage_limit: c.storage_limit,
            cpu_count: c.cpu_count,
            expose_port: c.expose_port,
            workload_spec: c.workload_spec.clone(),
            workload_identity,
            file_name: attach_name.or_else(|| c.file_name.clone()),
            original_score: c.original_score,
            min_score_rate: c.min_score_rate,
            difficulty: c.difficulty,
            score_curve: c.score_curve,
            enable_traffic_capture: c.enable_traffic_capture,
            enable_shared_container: c.enable_shared_container,
            disable_blood_bonus: c.disable_blood_bonus,
            network_mode: c.network_mode,
            attachment,
            test_container,
            build_status: c.build_status,
            last_build_log: c.last_build_log.clone(),
            ad_checker_image: c.ad_checker_image.clone(),
            ad_allow_egress: c.ad_allow_egress,
            ad_allow_self_reset: c.ad_allow_self_reset,
            ad_ssh_requires_flag: c.ad_ssh_requires_flag,
            ad_self_hosted: c.ad_self_hosted,
            ad_scoring_weight: c.ad_scoring_weight,
            flags,
        })
    }
}

/// RSCTF `Models/Request/Edit/FlagCreateModel` — a flag plus optional attachment
/// metadata (the attachment the flag hands out on solve).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlagCreateModel {
    pub flag: String,
    #[serde(default)]
    pub attachment_type: Option<FileType>,
    #[serde(default)]
    pub file_hash: Option<String>,
    #[serde(default)]
    pub remote_url: Option<String>,
}

/// RSCTF `Models/Request/Edit/AttachmentCreateModel` — new attachment for a
/// (non-dynamic) challenge's canonical download.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentCreateModel {
    #[serde(default)]
    pub attachment_type: Option<FileType>,
    #[serde(default)]
    pub file_hash: Option<String>,
    #[serde(default)]
    pub remote_url: Option<String>,
}

/// RSCTF `Models/Request/Edit/GameCloneModel` — deep-copy an existing game into
/// a new hidden template. Defaults mirror the C# (`+7d`/`+14d`, challenges on).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameCloneModel {
    #[serde(default)]
    pub title: String,
    #[serde(default = "epoch", with = "crate::utils::datetime::millis")]
    pub start_time_utc: DateTime<Utc>,
    #[serde(default = "epoch", with = "crate::utils::datetime::millis")]
    pub end_time_utc: DateTime<Utc>,
    #[serde(default = "default_true")]
    pub include_challenges: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameNoticeModel {
    #[serde(default)]
    pub content: String,
    #[serde(default, with = "crate::utils::datetime::millis_opt")]
    pub publish_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DivisionCreateModel {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub invite_code: Option<String>,
    #[serde(default)]
    pub default_permissions: Option<i32>,
    #[serde(default)]
    pub challenge_configs: Option<Vec<DivisionChallengeConfigInput>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DivisionEditModel {
    pub name: Option<String>,
    pub invite_code: Option<String>,
    pub default_permissions: Option<i32>,
    #[serde(default)]
    pub challenge_configs: Option<Vec<DivisionChallengeConfigInput>>,
}

/// Inbound half of RSCTF `DivisionChallengeConfigModel` — a per-challenge
/// permission override for a division. `permissions` is a numeric
/// `GamePermission` bit-set; defaults to `All` when omitted (matching the C#).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DivisionChallengeConfigInput {
    pub challenge_id: i32,
    #[serde(default)]
    pub permissions: Option<i32>,
}

/// RSCTF `Models/Request/Edit/RejectChallengeModel` — optional audit note.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectChallengeModel {
    #[serde(default)]
    pub note: Option<String>,
}

/// RSCTF `ChallengeReviewDetailModel.FromReview` — one persisted review row.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeReviewDetailModel {
    pub id: i32,
    pub challenge_id: i32,
    /// Resolved via a join on `game_challenge` (see `get_reviews`).
    pub challenge_name: Option<String>,
    /// Resolved via a join on `game` (see `get_reviews`).
    pub game_title: Option<String>,
    pub user_id: Uuid,
    /// Resolved via a join on `user` (see `get_reviews`).
    pub user_name: Option<String>,
    pub rating: ReviewRating,
    pub comment: Option<String>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub submit_time_utc: DateTime<Utc>,
}

impl ChallengeReviewDetailModel {
    fn from_review(
        r: &challenge_review::Model,
        challenge_name: Option<String>,
        game_title: Option<String>,
        user_name: Option<String>,
    ) -> Self {
        Self {
            id: r.id,
            challenge_id: r.challenge_id,
            challenge_name,
            game_title,
            user_id: r.user_id,
            user_name,
            rating: r.rating,
            comment: r.comment.clone(),
            submit_time_utc: r.submit_time_utc,
        }
    }
}

/// RSCTF `PendingChallengeModel` — a challenge sitting in the review queue.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingChallengeModel {
    pub id: i32,
    pub title: String,
    pub category: ChallengeCategory,
    #[serde(rename = "type")]
    pub challenge_type: ChallengeType,
    pub review_status: ChallengeReviewStatus,
    pub review_note: Option<String>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub submitted_at_utc: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub reviewed_at_utc: Option<DateTime<Utc>>,
    pub submitted_by_user_id: Option<Uuid>,
    /// Resolved via a join on `user` (see `list_pending_challenges`).
    pub submitted_by_user_name: Option<String>,
}

impl PendingChallengeModel {
    fn from_challenge(c: &game_challenge::Model) -> Self {
        Self {
            id: c.id,
            title: c.title.clone(),
            category: c.category,
            challenge_type: c.challenge_type,
            review_status: c.review_status,
            review_note: c.review_note.clone(),
            submitted_at_utc: c.submitted_at_utc,
            reviewed_at_utc: c.reviewed_at_utc,
            submitted_by_user_id: c.submitted_by_user_id,
            submitted_by_user_name: None,
        }
    }
}

// ============================================================================
//  Router
// ============================================================================

pub fn router() -> Router<SharedState> {
    Router::new()
        // --- Posts ---
        .route("/api/edit/posts", post(add_post))
        .route("/api/edit/posts/{id}", put(update_post).delete(delete_post))
        // --- Games ---
        .route("/api/edit/games", get(get_games).post(add_game))
        .route("/api/edit/games/import", post(import_game))
        .route(
            "/api/edit/games/{id}",
            get(get_game).put(update_game).delete(delete_game),
        )
        .route("/api/edit/games/{id}/HashSalt", get(get_hash_salt))
        .route("/api/edit/games/{id}/Clone", post(clone_game))
        .route("/api/edit/games/{id}/writeups", delete(delete_writeups))
        .route("/api/edit/games/{id}/poster", put(update_poster))
        .route("/api/edit/games/{id}/export", post(export_game))
        .route(
            "/api/edit/games/{id}/scoreboard/flush",
            post(flush_scoreboard),
        )
        // --- Admin delegation ---
        .route("/api/edit/games/{id}/admins", get(get_game_admins))
        .route(
            "/api/edit/games/{id}/admins/{userId}",
            post(add_game_admin).delete(remove_game_admin),
        )
        // --- Reviews / pending queue ---
        .route("/api/edit/games/{id}/reviews", get(get_reviews))
        .route(
            "/api/edit/games/{id}/reviews/analytics",
            get(get_review_analytics),
        )
        .route(
            "/api/edit/games/{id}/pendingchallenges",
            get(list_pending_challenges),
        )
        // --- Game challenges ---
        .route(
            "/api/edit/games/{id}/challenges",
            get(get_challenges).post(add_challenge),
        )
        .route(
            "/api/edit/games/{id}/challenges/submit",
            post(submit_challenge),
        )
        .route(
            "/api/edit/games/{id}/challenges/import",
            post(import_challenge),
        )
        .route(
            "/api/edit/games/{id}/challenges/importfromgithub",
            post(import_from_github),
        )
        .route(
            "/api/edit/games/{id}/challenges/{cId}",
            get(get_challenge)
                .put(update_challenge)
                .delete(delete_challenge),
        )
        .route(
            "/api/edit/games/{id}/challenges/{cId}/approve",
            post(approve_challenge),
        )
        .route(
            "/api/edit/games/{id}/challenges/{cId}/reject",
            post(reject_challenge),
        )
        .route(
            "/api/edit/games/{id}/challenges/{cId}/attachment",
            post(update_attachment),
        )
        .route(
            "/api/edit/games/{id}/challenges/{cId}/auditmeta",
            get(get_challenge_audit_meta),
        )
        .route(
            "/api/edit/games/{id}/challenges/{cId}/rebuild",
            post(rebuild_challenge),
        )
        .route(
            "/api/edit/games/{id}/challenges/{cId}/workload/rollout",
            post(rollout_workloads),
        )
        .route(
            "/api/edit/games/{id}/challenges/{cId}/container",
            post(create_test_container).delete(destroy_test_container),
        )
        .route(
            "/api/edit/games/{id}/challenges/{cId}/flags",
            post(add_flags),
        )
        .route(
            "/api/edit/games/{id}/challenges/{cId}/flags/{fId}",
            delete(remove_flag),
        )
        // --- Notices ---
        .route(
            "/api/edit/games/{id}/notices",
            get(get_notices).post(add_notice),
        )
        .route(
            "/api/edit/games/{id}/notices/{noticeId}",
            put(update_notice).delete(delete_notice),
        )
        // --- Divisions ---
        .route(
            "/api/edit/games/{id}/divisions",
            get(get_divisions).post(create_division),
        )
        .route(
            "/api/edit/games/{id}/divisions/{divisionId}",
            put(update_division).delete(delete_division),
        )
        // --- Attack & Defense live console (infra; valid-empty responses) ---
        .route(
            "/api/edit/games/{id}/ad/AdvanceRound",
            post(ad_advance_round),
        )
        .route("/api/edit/games/{id}/ad/State", get(ad_state))
        .route(
            "/api/edit/games/{id}/ad/EnsureContainers",
            post(ad_ensure_containers),
        )
        .route(
            "/api/edit/games/{id}/ad/ScoringPause",
            post(ad_scoring_pause),
        )
        .route(
            "/api/edit/games/{id}/ad/Challenges/{challengeId}/Toggle",
            post(ad_toggle_challenge),
        )
        .route(
            "/api/edit/games/{id}/ad/Checks/{checkId}/Override",
            post(ad_override_check),
        )
        .route(
            "/api/edit/games/{id}/ad/Services/{adTeamServiceId}/File",
            get(ad_service_file),
        )
        .route(
            "/api/edit/games/{id}/ad/Services/{adTeamServiceId}/Inspector",
            post(ad_spawn_inspector),
        )
        .route(
            "/api/edit/games/{id}/ad/Services/{adTeamServiceId}/Inspector/{containerGuid}",
            delete(ad_destroy_inspector),
        )
        .route(
            "/api/edit/games/{id}/ad/Services/{adTeamServiceId}/Restart",
            post(ad_restart_service),
        )
        .route(
            "/api/edit/games/{id}/ad/Services/{adTeamServiceId}/Snapshot",
            get(ad_download_snapshot),
        )
        .route(
            "/api/edit/games/{id}/ad/Services/{adTeamServiceId}/Snapshot/Changes",
            get(ad_snapshot_changes),
        )
        .route(
            "/api/edit/games/{id}/ad/Services/{adTeamServiceId}/SnapshotDiff",
            get(ad_snapshot_diff),
        )
        .route(
            "/api/edit/games/{id}/ad/Services/{adTeamServiceId}/Snapshots",
            get(ad_service_snapshots),
        )
}

// ============================================================================
//  Authorization helper
// ============================================================================

/// Authorize a game-scoped edit. Succeeds when the caller is a platform admin,
/// or a co-organizer (a `game_manager` row exists for `(game_id, user.id)`).
/// Mirrors RSCTF `[RequireGameAdmin]` (`EventManager` membership OR the `Admin`
/// role), letting a manager edit THEIR game without platform-wide admin.
async fn manager_or_admin(
    st: &SharedState,
    user: &CurrentUser,
    game_id: i32,
) -> Result<(), AppError> {
    if user.is_admin() {
        return Ok(());
    }
    let is_manager = game_manager::Entity::find()
        .filter(game_manager::Column::GameId.eq(game_id))
        .filter(game_manager::Column::UserId.eq(user.id))
        .count(&st.db)
        .await?
        > 0;
    if is_manager {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

/// Co-organizer view of a user (RSCTF `UserInfoModel`). The manager-list route is
/// typed `ProfileUserInfoModel[]` on the client, so the camelCase field set
/// mirrors that shape (`userId`/`userName`/`stdNumber`/`hasManagedGames`, ...).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerInfoModel {
    pub user_id: Uuid,
    pub user_name: Option<String>,
    pub email: Option<String>,
    pub role: Role,
    pub bio: String,
    pub real_name: String,
    pub std_number: String,
    pub phone: Option<String>,
    pub avatar: Option<String>,
    pub has_managed_games: bool,
}

impl ManagerInfoModel {
    fn from_user(u: &user::Model) -> Self {
        Self {
            user_id: u.id,
            user_name: u.user_name.clone(),
            email: u.email.clone(),
            role: u.role,
            bio: u.bio.clone(),
            real_name: u.real_name.clone(),
            std_number: u.std_number.clone(),
            phone: u.phone_number.clone(),
            avatar: u.avatar_url(),
            has_managed_games: true,
        }
    }
}

// ============================================================================
//  Posts
// ============================================================================

mod ad;
mod builds;
mod challenges;
mod divisions;
mod flags;
mod games;
mod helpers;
mod notices;
mod posts;
mod reviews;
mod test_container;
mod transfer;

pub use ad::*;
pub use builds::backfill_build_records;
pub(crate) use builds::*;
pub use challenges::*;
pub use divisions::*;
pub use flags::*;
pub use games::*;
pub(crate) use helpers::*;
pub use notices::*;
pub use posts::*;
pub use reviews::*;
pub use test_container::*;
pub use transfer::*;

#[cfg(test)]
mod request_model_tests {
    use super::ChallengeUpdateModel;

    #[test]
    fn workload_update_distinguishes_missing_from_null() {
        let unchanged: ChallengeUpdateModel = serde_json::from_str("{}").unwrap();
        assert!(unchanged.workload_spec.is_none());

        let cleared: ChallengeUpdateModel =
            serde_json::from_str(r#"{"workloadSpec":null}"#).unwrap();
        assert!(matches!(cleared.workload_spec, Some(None)));
    }
}
