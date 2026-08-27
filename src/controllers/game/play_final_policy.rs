//! Final, transaction-retained authorization for private play responses.
//!
//! The normal play loaders deliberately use short-lived caches. This boundary
//! runs after every expensive read, on the same transaction as the live-roster
//! fence, so a committed editor revoke can never lose to stale response data.

use std::collections::{BTreeSet, HashMap};

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlayAccessPhase {
    Live,
    Archived,
}

#[derive(Debug)]
struct LockedPlayScope {
    practice_mode: bool,
    start_time_utc: DateTime<Utc>,
    end_time_utc: DateTime<Utc>,
}

type LockedPlayScopeRow = (bool, DateTime<Utc>, DateTime<Utc>, Option<i32>);

#[derive(Clone, Copy)]
pub(super) struct ChallengeResponseScope {
    game_id: i32,
    team_id: i32,
    participation_id: i32,
    challenge_id: i32,
}

impl ChallengeResponseScope {
    pub(super) fn new(
        game_id: i32,
        team_id: i32,
        participation_id: i32,
        challenge_id: i32,
    ) -> Self {
        Self {
            game_id,
            team_id,
            participation_id,
            challenge_id,
        }
    }
}

impl LockedPlayScope {
    /// Read the database clock only after every potentially-blocking advisory
    /// and row lock has been acquired. A request queued behind an editor or
    /// another ChallengeOpened writer can therefore never carry a stale live
    /// decision across the event deadline.
    async fn phase_at_db_clock(
        &self,
        connection: &mut sqlx::PgConnection,
    ) -> AppResult<PlayAccessPhase> {
        let observed_at: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(connection)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        if observed_at < self.start_time_utc {
            return Err(AppError::game_not_started());
        }
        Ok(if !self.practice_mode && observed_at >= self.end_time_utc {
            PlayAccessPhase::Archived
        } else {
            PlayAccessPhase::Live
        })
    }
}

#[derive(Debug)]
enum PreparedAttachment {
    NotEmitted,
    Observed {
        attachment: Option<attachment::Model>,
        local_file: Option<local_file::Model>,
    },
}

#[derive(Debug)]
enum PreparedRuntime {
    None,
    PerTeam {
        instance: game_instance::Model,
        container: container::Model,
    },
    Shared {
        container: container::Model,
    },
}

/// Exact database identities behind a prepared private challenge model. The
/// response DTO intentionally omits these ids, so carrying this server-only
/// grant is what lets the final transaction reject a detached remote URL or a
/// rotated container endpoint instead of leaking the stale early projection.
#[derive(Debug)]
struct PreparedChallenge {
    id: i32,
    title: String,
    content: String,
    category: i16,
    challenge_type: i16,
    hints: Option<Json>,
    attachment_id: Option<i32>,
    submission_limit: i32,
    deadline_utc: Option<DateTime<Utc>>,
    enable_shared_container: bool,
    workload_spec: Option<Json>,
    container_image: Option<String>,
    expose_port: Option<i32>,
    shared_container_id: Option<Uuid>,
}

#[derive(Debug)]
pub(super) struct PreparedChallengeGrant {
    challenge: PreparedChallenge,
    attachment: PreparedAttachment,
    runtime: PreparedRuntime,
}

impl PreparedChallengeGrant {
    pub(super) fn new(challenge: &game_challenge::Model) -> Self {
        Self {
            challenge: PreparedChallenge {
                id: challenge.id,
                title: challenge.title.clone(),
                content: challenge.content.clone(),
                category: challenge.category as i16,
                challenge_type: challenge.challenge_type as i16,
                hints: challenge.hints.clone(),
                attachment_id: challenge.attachment_id,
                submission_limit: challenge.submission_limit,
                deadline_utc: challenge.deadline_utc,
                enable_shared_container: challenge.enable_shared_container,
                workload_spec: challenge.workload_spec.clone(),
                container_image: challenge.container_image.clone(),
                expose_port: challenge.expose_port,
                shared_container_id: challenge.shared_container_id,
            },
            attachment: PreparedAttachment::NotEmitted,
            runtime: PreparedRuntime::None,
        }
    }

    pub(super) fn bind_attachment(
        &mut self,
        attachment: Option<attachment::Model>,
        local_file: Option<local_file::Model>,
    ) {
        self.attachment = PreparedAttachment::Observed {
            attachment,
            local_file,
        };
    }

    pub(super) fn bind_per_team_runtime(
        &mut self,
        instance: game_instance::Model,
        container: container::Model,
    ) {
        self.runtime = PreparedRuntime::PerTeam {
            instance,
            container,
        };
    }

    pub(super) fn bind_shared_runtime(&mut self, container: container::Model) {
        self.runtime = PreparedRuntime::Shared { container };
    }

    fn matches_response_projection(&self, model: &ChallengeDetailModel) -> bool {
        let attachment_matches = match &self.attachment {
            PreparedAttachment::NotEmitted => {
                model.context.url.is_none()
                    && model.context.file_size.is_none()
                    && model.context.sha256.is_none()
            }
            PreparedAttachment::Observed {
                attachment: None,
                local_file: None,
            } => {
                model.context.url.is_none()
                    && model.context.file_size.is_none()
                    && model.context.sha256.is_none()
            }
            PreparedAttachment::Observed {
                attachment: Some(attachment),
                local_file,
            } => match attachment.file_type {
                FileType::Remote => {
                    model.context.url == attachment.remote_url
                        && model.context.file_size.is_none()
                        && model.context.sha256.is_none()
                }
                FileType::Local => match local_file {
                    Some(file) => {
                        model.context.url == Some(format!("/assets/{}/{}", file.hash, file.name))
                            && model.context.file_size == Some(file.file_size)
                            && model.context.sha256.as_deref() == Some(file.hash.as_str())
                    }
                    None => {
                        model.context.url.is_none()
                            && model.context.file_size.is_none()
                            && model.context.sha256.is_none()
                    }
                },
                FileType::None => {
                    model.context.url.is_none()
                        && model.context.file_size.is_none()
                        && model.context.sha256.is_none()
                }
            },
            PreparedAttachment::Observed {
                attachment: None,
                local_file: Some(_),
            } => false,
        };
        let runtime_matches = match &self.runtime {
            PreparedRuntime::None => {
                model.context.instance_id.is_none()
                    && model.context.instance_entry.is_none()
                    && model.context.close_time.is_none()
            }
            PreparedRuntime::PerTeam {
                instance: _,
                container,
            } => {
                !model.context.is_shared_instance
                    && model.context.instance_id == Some(container.id)
                    && model
                        .context
                        .instance_entry
                        .as_ref()
                        .is_some_and(|entry| entry == &container.entry())
                    && model.context.close_time == Some(container.expect_stop_at)
            }
            PreparedRuntime::Shared { container } => {
                model.context.is_shared_instance
                    && model.context.instance_id == Some(container.id)
                    && model
                        .context
                        .instance_entry
                        .as_ref()
                        .is_some_and(|entry| entry == &container.entry())
                    && model.context.close_time == Some(container.expect_stop_at)
            }
        };
        attachment_matches && runtime_matches
    }
}

/// Lock the exact game/participation, current division policy, and every
/// challenge that will be emitted. Official editor paths update the parent
/// Division row before replacing overrides, so its shared row lock also closes
/// the otherwise-unlockable "missing override gets inserted" race.
async fn lock_play_scope_on(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    team_id: i32,
    participation_id: i32,
    challenge_ids: &[i32],
) -> AppResult<LockedPlayScope> {
    let scope: Option<LockedPlayScopeRow> = sqlx::query_as(
        r#"SELECT game.practice_mode, game.start_time_utc, game.end_time_utc,
                  participation.division_id
             FROM "Games" game
             JOIN "Participations" participation
               ON participation.game_id = game.id
              AND participation.id = $2
              AND participation.team_id = $3
            WHERE game.id = $1
              AND game.deletion_pending = FALSE
              AND participation.status = $4
            FOR SHARE OF game, participation"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .bind(team_id)
    .bind(ParticipationStatus::Accepted as i16)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((practice_mode, start_time_utc, end_time_utc, division_id)) = scope else {
        return Err(AppError::Forbidden);
    };

    // Callers can pass scoreboard-derived ids in arbitrary category order.
    // De-duplicate before comparing the exact authorized database set.
    let expected: Vec<i32> = challenge_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    // Division policy writers always lock/update this parent before replacing
    // its override rows. Take it before challenge rows to preserve the global
    // game -> division -> challenge read order.
    let division_default = if let Some(division_id) = division_id {
        let stored: Option<i32> = sqlx::query_scalar(
            r#"SELECT default_permissions
                 FROM "Divisions"
                WHERE id = $1 AND game_id = $2
                FOR SHARE"#,
        )
        .bind(division_id)
        .bind(game_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        Some(stored.ok_or_else(|| AppError::not_found("Challenge not found"))?)
    } else {
        None
    };

    if !expected.is_empty() {
        let playable: Vec<i32> = sqlx::query_scalar(
            r#"SELECT challenge.id
             FROM "GameChallenges" challenge
            WHERE challenge.game_id = $1
              AND challenge.id = ANY($2)
              AND challenge.is_enabled = TRUE
              AND challenge.review_status = $3
              AND challenge.deletion_pending = FALSE
            ORDER BY challenge.id
            FOR SHARE OF challenge"#,
        )
        .bind(game_id)
        .bind(&expected)
        .bind(ChallengeReviewStatus::Active as i16)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if playable != expected {
            return Err(AppError::not_found("Challenge not found"));
        }
    }

    if let (Some(division_id), Some(default_permissions)) = (division_id, division_default) {
        let overrides: HashMap<i32, i32> = sqlx::query_as::<_, (i32, i32)>(
            r#"SELECT challenge_id, permissions
                 FROM "DivisionChallengeConfigs"
                WHERE division_id = $1 AND challenge_id = ANY($2)
                FOR SHARE"#,
        )
        .bind(division_id)
        .bind(&expected)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .into_iter()
        .collect();
        let every_challenge_visible = expected.iter().all(|challenge_id| {
            GamePermission(
                overrides
                    .get(challenge_id)
                    .copied()
                    .unwrap_or(default_permissions),
            )
            .contains(GamePermission::VIEW_CHALLENGE)
        });
        if !every_challenge_visible {
            return Err(AppError::not_found("Challenge not found"));
        }
    }

    Ok(LockedPlayScope {
        practice_mode,
        start_time_utc,
        end_time_utc,
    })
}

#[derive(sqlx::FromRow)]
struct ChallengePayloadRow {
    title: String,
    content: String,
    category: i16,
    challenge_type: i16,
    hints: Option<Json>,
    attachment_id: Option<i32>,
    submission_limit: i32,
    deadline_utc: Option<DateTime<Utc>>,
    enable_shared_container: bool,
    workload_spec: Option<Json>,
    container_image: Option<String>,
    expose_port: Option<i32>,
    shared_container_id: Option<Uuid>,
}

#[derive(sqlx::FromRow)]
struct AttachmentRow {
    id: i32,
    file_type: i16,
    remote_url: Option<String>,
    local_file_id: Option<i32>,
}

#[derive(sqlx::FromRow)]
struct FileRow {
    id: i32,
    hash: String,
    file_size: i64,
    name: String,
}

#[derive(sqlx::FromRow)]
struct InstanceRow {
    id: i32,
    challenge_id: i32,
    participation_id: i32,
    is_loaded: bool,
    last_container_operation: DateTime<Utc>,
    flag_id: Option<i32>,
    container_id: Option<Uuid>,
}

#[derive(sqlx::FromRow)]
struct ContainerRow {
    id: Uuid,
    container_id: String,
    status: i16,
    expect_stop_at: DateTime<Utc>,
    is_proxy: bool,
    ip: String,
    port: i32,
    public_ip: Option<String>,
    public_port: Option<i32>,
    game_instance_id: Option<i32>,
}

fn challenge_payload_matches(current: &ChallengePayloadRow, expected: &PreparedChallenge) -> bool {
    current.title == expected.title
        && current.content == expected.content
        && current.category == expected.category
        && current.challenge_type == expected.challenge_type
        && current.hints == expected.hints
        && current.attachment_id == expected.attachment_id
        && current.submission_limit == expected.submission_limit
        && current.deadline_utc == expected.deadline_utc
        && current.enable_shared_container == expected.enable_shared_container
        && current.workload_spec == expected.workload_spec
        && current.container_image == expected.container_image
        && current.expose_port == expected.expose_port
        && current.shared_container_id == expected.shared_container_id
}

fn container_matches(current: &ContainerRow, expected: &container::Model) -> bool {
    current.id == expected.id
        && current.container_id == expected.container_id
        && current.status == expected.status as i16
        && current.expect_stop_at == expected.expect_stop_at
        && current.is_proxy == expected.is_proxy
        && current.ip == expected.ip
        && current.port == expected.port
        && current.public_ip == expected.public_ip
        && current.public_port == expected.public_port
        && current.game_instance_id == expected.game_instance_id
}

async fn lock_attachment_matches_on(
    connection: &mut sqlx::PgConnection,
    expected_attachment_id: Option<i32>,
    prepared: &PreparedAttachment,
) -> AppResult<bool> {
    let PreparedAttachment::Observed {
        attachment: prepared_attachment,
        local_file: prepared_file,
    } = prepared
    else {
        return Ok(true);
    };

    let Some(attachment_id) = expected_attachment_id else {
        return Ok(prepared_attachment.is_none() && prepared_file.is_none());
    };
    let current_attachment = sqlx::query_as::<_, AttachmentRow>(
        r#"SELECT id, "Type" AS file_type, remote_url, local_file_id
             FROM "Attachments"
            WHERE id = $1
            FOR SHARE"#,
    )
    .bind(attachment_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let (current_attachment, prepared_attachment) =
        match (current_attachment.as_ref(), prepared_attachment.as_ref()) {
            (Some(current), Some(prepared)) => (current, prepared),
            (None, None) => return Ok(true),
            _ => return Ok(false),
        };
    if current_attachment.id != prepared_attachment.id
        || current_attachment.file_type != prepared_attachment.file_type as i16
        || current_attachment.remote_url != prepared_attachment.remote_url
        || current_attachment.local_file_id != prepared_attachment.local_file_id
    {
        return Ok(false);
    }

    let Some(file_id) = prepared_attachment.local_file_id else {
        return Ok(prepared_file.is_none());
    };
    let current_file = sqlx::query_as::<_, FileRow>(
        r#"SELECT id, hash, file_size, name
             FROM "Files"
            WHERE id = $1
            FOR SHARE"#,
    )
    .bind(file_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let (current_file, prepared_file) = match (current_file.as_ref(), prepared_file.as_ref()) {
        (Some(current), Some(prepared)) => (current, prepared),
        (None, None) => return Ok(true),
        _ => return Ok(false),
    };
    Ok(current_file.id == prepared_file.id
        && current_file.hash == prepared_file.hash
        && current_file.file_size == prepared_file.file_size
        && current_file.name == prepared_file.name)
}

async fn lock_container_on(
    connection: &mut sqlx::PgConnection,
    expected: &container::Model,
) -> AppResult<Option<ContainerRow>> {
    sqlx::query_as::<_, ContainerRow>(
        r#"SELECT id, container_id, status, expect_stop_at, is_proxy,
                  ip, port, public_ip, public_port, game_instance_id
             FROM "Containers"
            WHERE id = $1
            FOR SHARE"#,
    )
    .bind(expected.id)
    .fetch_optional(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn lock_runtime_matches_on(
    connection: &mut sqlx::PgConnection,
    participation_id: i32,
    challenge_id: i32,
    prepared: &PreparedRuntime,
) -> AppResult<bool> {
    match prepared {
        PreparedRuntime::None => Ok(true),
        PreparedRuntime::PerTeam {
            instance,
            container,
        } => {
            let current_instance = sqlx::query_as::<_, InstanceRow>(
                r#"SELECT id, challenge_id, participation_id, is_loaded,
                          last_container_operation, flag_id, container_id
                     FROM "GameInstances"
                    WHERE id = $1 AND participation_id = $2 AND challenge_id = $3
                    FOR SHARE"#,
            )
            .bind(instance.id)
            .bind(participation_id)
            .bind(challenge_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            let Some(current_instance) = current_instance else {
                return Ok(false);
            };
            if current_instance.id != instance.id
                || current_instance.challenge_id != instance.challenge_id
                || current_instance.participation_id != instance.participation_id
                || current_instance.is_loaded != instance.is_loaded
                || current_instance.last_container_operation != instance.last_container_operation
                || current_instance.flag_id != instance.flag_id
                || current_instance.container_id != instance.container_id
            {
                return Ok(false);
            }
            let current_container = lock_container_on(connection, container).await?;
            Ok(current_container.is_some_and(|current| {
                current.game_instance_id == Some(instance.id)
                    && container_matches(&current, container)
            }))
        }
        PreparedRuntime::Shared { container } => {
            let current_container = lock_container_on(connection, container).await?;
            Ok(current_container.is_some_and(|current| container_matches(&current, container)))
        }
    }
}

/// Lock and compare every private value projected before the final transaction.
/// Runtime mismatch is returned separately: an ended game strips it and may
/// still return the archive, while a live game must fail closed.
async fn lock_challenge_payload_on(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    participation_id: i32,
    grant: &PreparedChallengeGrant,
) -> AppResult<bool> {
    let current = sqlx::query_as::<_, ChallengePayloadRow>(
        r#"SELECT title, content, category, "Type" AS challenge_type, hints,
                  attachment_id, submission_limit, deadline_utc,
                  enable_shared_container, workload_spec, container_image,
                  expose_port, shared_container_id
             FROM "GameChallenges"
            WHERE id = $1 AND game_id = $2
              AND is_enabled = TRUE
              AND review_status = $3
              AND deletion_pending = FALSE
            FOR SHARE"#,
    )
    .bind(grant.challenge.id)
    .bind(game_id)
    .bind(ChallengeReviewStatus::Active as i16)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(current) = current else {
        return Err(AppError::not_found("Challenge not found"));
    };
    if !challenge_payload_matches(&current, &grant.challenge)
        || !lock_attachment_matches_on(connection, grant.challenge.attachment_id, &grant.attachment)
            .await?
    {
        return Err(AppError::not_found("Challenge changed; retry the request"));
    }
    lock_runtime_matches_on(
        connection,
        participation_id,
        grant.challenge.id,
        &grant.runtime,
    )
    .await
}

async fn release_with_result<T>(
    roster: crate::utils::single_flight::PgAdvisoryLock,
    result: AppResult<T>,
) -> AppResult<T> {
    let release = roster.release().await;
    match result {
        Ok(value) => {
            release.map_err(|error| AppError::internal(error.to_string()))?;
            Ok(value)
        }
        Err(error) => {
            if let Err(release_error) = release {
                tracing::warn!(
                    error = %release_error,
                    "failed to close denied play-response authorization fence"
                );
            }
            Err(error)
        }
    }
}

pub(super) async fn finish_details_response(
    pool: &sqlx::PgPool,
    user: &CurrentUser,
    game_id: i32,
    team_id: i32,
    participation_id: i32,
    challenge_ids: Vec<i32>,
    model: GameDetailModel,
) -> AppResult<Response> {
    let Some(mut roster) = crate::services::live_roster::try_acquire_participation_fence(
        pool,
        user.id,
        &user.security_stamp,
        game_id,
        team_id,
        participation_id,
        true,
    )
    .await?
    else {
        return Err(AppError::Forbidden);
    };

    let result = async {
        let scope = lock_play_scope_on(
            roster.transaction_mut(),
            game_id,
            team_id,
            participation_id,
            &challenge_ids,
        )
        .await?;
        scope.phase_at_db_clock(roster.transaction_mut()).await?;
        Ok(RequestResponse::ok(model).into_response())
    }
    .await;
    release_with_result(roster, result).await
}

fn strip_live_runtime_context(model: &mut ChallengeDetailModel) {
    model.context.instance_id = None;
    model.context.instance_entry = None;
    model.context.close_time = None;
    model.context.is_shared_instance = false;
}

pub(super) async fn finish_challenge_response(
    pool: &sqlx::PgPool,
    events: &crate::services::event_bus::EventBus,
    user: &CurrentUser,
    scope: ChallengeResponseScope,
    grant: PreparedChallengeGrant,
    mut model: ChallengeDetailModel,
) -> AppResult<Response> {
    let ChallengeResponseScope {
        game_id,
        team_id,
        participation_id,
        challenge_id,
    } = scope;
    if grant.challenge.id != challenge_id
        || model.id != challenge_id
        || !grant.matches_response_projection(&model)
    {
        return Err(AppError::internal(
            "prepared challenge response does not match its authorization grant",
        ));
    }
    let Some(mut roster) = crate::services::live_roster::try_acquire_participation_fence(
        pool,
        user.id,
        &user.security_stamp,
        game_id,
        team_id,
        participation_id,
        true,
    )
    .await?
    else {
        return Err(AppError::Forbidden);
    };

    // Take this before the clock snapshot. Another request can legitimately be
    // serializing the same first-open event; if that wait crosses the deadline,
    // the phase read below must observe Archived.
    let event_key = format!("challenge-opened:{game_id}:{team_id}:{challenge_id}");
    if let Err(error) = roster.acquire_additional(&event_key).await {
        return release_with_result(roster, Err(AppError::internal(error.to_string()))).await;
    }

    let result = async {
        let mut inserted_event_id = None;
        let scope = lock_play_scope_on(
            roster.transaction_mut(),
            game_id,
            team_id,
            participation_id,
            &[challenge_id],
        )
        .await?;
        let runtime_matches =
            lock_challenge_payload_on(roster.transaction_mut(), game_id, participation_id, &grant)
                .await?;
        let mut phase = scope.phase_at_db_clock(roster.transaction_mut()).await?;
        if phase == PlayAccessPhase::Live && !runtime_matches {
            // A teardown may have removed the runtime just as the game ended.
            // Re-read the database clock before denying an archive that no
            // longer contains any runtime coordinate.
            phase = scope.phase_at_db_clock(roster.transaction_mut()).await?;
            if phase == PlayAccessPhase::Live {
                return Err(AppError::not_found("Challenge runtime changed; retry"));
            }
        }
        if phase == PlayAccessPhase::Archived {
            // Static attachment metadata remains useful in the read-only
            // archive. Only runtime endpoints and their lifecycle marker are
            // live-only.
            strip_live_runtime_context(&mut model);
        } else {
            let challenge_id_text = challenge_id.to_string();
            inserted_event_id = sqlx::query_scalar(
                r#"INSERT INTO "GameEvents"
                     (game_id, "Type", "values", publish_time_utc, user_id, team_id)
                   SELECT $1, $2, $3, clock_timestamp(), $4, $5
                     FROM "Games" game
                    WHERE game.id = $1
                      AND game.deletion_pending = FALSE
                      AND game.start_time_utc <= clock_timestamp()
                      AND (game.practice_mode OR game.end_time_utc > clock_timestamp())
                      AND NOT EXISTS (
                          SELECT 1 FROM "GameEvents"
                           WHERE game_id = $1 AND team_id = $5 AND "Type" = $2
                             AND "values"->>0 = $6
                    )
                   RETURNING id"#,
            )
            .bind(game_id)
            .bind(EventType::ChallengeOpened as i16)
            .bind(serde_json::json!([
                challenge_id_text.clone(),
                model.title.clone()
            ]))
            .bind(user.id)
            .bind(team_id)
            .bind(&challenge_id_text)
            .fetch_optional(&mut **roster.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;

            // The insert itself has a DB-clock live predicate. Re-read once
            // more immediately before serialization so a deadline crossed in
            // that statement also strips the prepared endpoint.
            phase = scope.phase_at_db_clock(roster.transaction_mut()).await?;
            if phase == PlayAccessPhase::Archived {
                strip_live_runtime_context(&mut model);
            }
        }
        Ok((
            RequestResponse::ok(model).into_response(),
            inserted_event_id,
        ))
    }
    .await;
    let (response, event_id) = release_with_result(roster, result).await?;
    if let Some(event_id) = event_id {
        if let Err(error) =
            crate::services::game_event_feed::publish_committed_on(pool, events, &[event_id]).await
        {
            tracing::warn!(event_id, %error, "challenge-open event publish failed");
        }
    }
    Ok(response)
}

#[cfg(test)]
#[path = "play_final_policy_tests.rs"]
mod tests;
