//! Player challenge opening and detail projection.

use super::*;

// Challenge view + submission
// ---------------------------------------------------------------------------

/// `POST /api/game/{id}/challenge/{challengeId}/open` — unlock a challenge.
pub async fn open_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<StatusCode> {
    // RSCTF marks the challenge as opened for the team; rsctf exposes every
    // enabled challenge to accepted participants, so this is a no-op gate check.
    let ctx = context_info(&st, &user, id, true).await?;
    load_playable_challenge(&st, id, challenge_id).await?;
    let perm = effective_permission(&st, &ctx.participation, challenge_id).await?;
    if !perm.contains(GamePermission::VIEW_CHALLENGE) {
        return Err(AppError::not_found("Challenge not found"));
    }
    Ok(StatusCode::OK)
}

/// `GET /api/game/{id}/challenges/{challengeId}` — player challenge view.
pub async fn get_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<Response> {
    // Challenge content, hints, static attachments, final score, and solvers
    // remain readable after closeout. Operational context is stripped below.
    let ctx = context_info(&st, &user, id, false).await?;

    let challenge = load_playable_challenge(&st, id, challenge_id).await?;
    let variant = if challenge.variant_mode == ChallengeVariantMode::PerParticipation {
        Some(
            crate::services::event_security::variant_for_participation(
                &st,
                id,
                challenge_id,
                ctx.participation.id,
            )
            .await?
            .ok_or_else(|| {
                AppError::unavailable(
                    "This participation's deterministic challenge variant is not ready",
                )
            })?,
        )
    } else {
        None
    };
    let variant_manifest = variant
        .as_ref()
        .map(|row| crate::services::event_security::decode_manifest(&row.manifest))
        .transpose()?;
    let mut response_grant = final_policy::PreparedChallengeGrant::new(&challenge);

    // Division may restrict viewing this challenge (RSCTF GetChallenge gate):
    // lacking ViewChallenge hides it as a 404, mirroring the submit gate.
    let perm = effective_permission(&st, &ctx.participation, challenge_id).await?;
    if !perm.contains(GamePermission::VIEW_CHALLENGE) {
        return Err(AppError::not_found("Challenge not found"));
    }

    let mut context = ClientFlagContext {
        participation_id: Some(ctx.participation.id),
        ..Default::default()
    };

    // Per-team instance -> running container connection entry.
    if !ctx.archived {
        if let Some(instance) = game_instance::Entity::find()
            .filter(game_instance::Column::ParticipationId.eq(ctx.participation.id))
            .filter(game_instance::Column::ChallengeId.eq(challenge_id))
            .one(&st.db)
            .await?
        {
            if let Some(cont) = container::Entity::find()
                .filter(container::Column::GameInstanceId.eq(instance.id))
                .one(&st.db)
                .await?
            {
                context.instance_id = Some(cont.id);
                context.instance_entry = Some(cont.entry());
                context.close_time = Some(cont.expect_stop_at);
                response_grant.bind_per_team_runtime(instance, cont);
            }
        }
    }

    // Static attachment URL. Mirrors RSCTF `GameInstance.AttachmentUrl =
    // Challenge.Attachment.UrlWithName()`: resolve the challenge's attachment to
    // its LocalFile and emit the hash-addressed `/assets/{hash}/{name}` URL that
    // `AssetsController` serves (remote attachments surface their raw URL). The
    // previous `/assets/download/{id}/{name}` form had no matching route and hit
    // the SPA fallback (200 HTML). Dynamic-attachment per-flag files live on the
    // flag context, which this port never populates, so only the challenge-owned
    // attachment is resolved here.
    if context.instance_entry.is_none() {
        let prepared_attachment = if let Some(att_id) = challenge.attachment_id {
            attachment::Entity::find_by_id(att_id).one(&st.db).await?
        } else {
            None
        };
        let prepared_file = if let Some(att) = prepared_attachment.as_ref() {
            if let Some(local_file_id) = att.local_file_id {
                local_file::Entity::find_by_id(local_file_id)
                    .one(&st.db)
                    .await?
            } else {
                None
            }
        } else {
            None
        };
        if let Some(att) = prepared_attachment.as_ref() {
            match att.file_type {
                FileType::Remote => context.url = att.remote_url.clone(),
                FileType::Local => {
                    if let Some(lf) = prepared_file.as_ref() {
                        context.url = Some(format!("/assets/{}/{}", lf.hash, lf.name));
                        context.file_size = Some(lf.file_size);
                        context.sha256 = Some(lf.hash.clone());
                    }
                }
                FileType::None => {}
            }
        }
        response_grant.bind_attachment(prepared_attachment, prepared_file);
    }

    // Shared container: the challenge serves ONE container to every team, so the
    // team's own instance owns no container — surface the challenge-owned shared
    // container's connection (read-only for players; only an admin can stop it).
    // Mirrors RSCTF `GameController.GetChallenge` (UsesSharedContainer branch): sets
    // IsSharedInstance and overrides Entry/CloseTime while leaving any attachment Url.
    if !ctx.archived && uses_shared_container(&challenge) {
        context.is_shared_instance = true;
        if let Some(sid) = challenge.shared_container_id {
            if let Some(shared) = container::Entity::find_by_id(sid).one(&st.db).await? {
                context.instance_id = Some(shared.id);
                context.instance_entry = Some(shared.entry());
                context.close_time = Some(shared.expect_stop_at);
                response_grant.bind_shared_runtime(shared);
            }
        }
    }

    // Attempts so far for this participation+challenge.
    let attempts = submission::Entity::find()
        .filter(submission::Column::ParticipationId.eq(ctx.participation.id))
        .filter(submission::Column::ChallengeId.eq(challenge_id))
        .count(&st.db)
        .await? as i32;

    // Caller's own review of this challenge, if any (RSCTF surfaces this so the
    // player UI can pre-fill the like/dislike + comment controls).
    let review = challenge_review::Entity::find()
        .filter(challenge_review::Column::UserId.eq(user.id))
        .filter(challenge_review::Column::ChallengeId.eq(challenge_id))
        .one(&st.db)
        .await?;
    let (user_rating, user_comment) = match review {
        Some(r) => (r.rating, r.comment),
        None => (ReviewRating::None, None),
    };

    // Project the score from the same board snapshot used by `/details` and the
    // solver list. In particular, a public viewer during the freeze must not learn
    // post-freeze solve activity by polling this modal's dynamic score.
    let board = build_scoreboard_cached(&st, &ctx.game, user.is_monitor()).await?;
    let current_score = board
        .challenges
        .values()
        .flatten()
        .find(|info| info.id == challenge_id)
        .map(|info| info.score)
        // The challenge passed the live visibility gate above. A miss can only be
        // a short-lived cache transition after an organizer edit; zero is the safe
        // non-leaking value until the five-second snapshot refreshes.
        .unwrap_or(0);

    let model = ChallengeDetailModel {
        id: challenge.id,
        title: challenge.title,
        content: variant_manifest
            .as_ref()
            .and_then(|manifest| manifest.content.clone())
            .unwrap_or(challenge.content),
        category: challenge.category,
        challenge_type: challenge.challenge_type,
        hints: variant_manifest
            .as_ref()
            .and_then(|manifest| manifest.hints.as_ref())
            .map(|hints| serde_json::json!(hints))
            .or(challenge.hints),
        score: current_score,
        context,
        limit: challenge.submission_limit,
        attempts,
        deadline: challenge.deadline_utc,
        user_rating,
        user_comment,
        solve_receipt_mode: challenge.solve_receipt_mode,
        receipt_verifier_identity: challenge.receipt_verifier_identity,
        ad_self_hosted: challenge.ad_self_hosted,
        variant: variant.map(|row| ClientChallengeVariant {
            id: row.id,
            revision: row.revision,
            artifact_hash: hex::encode(row.artifact_hash),
        }),
    };

    // Final authority, current game/challenge/division policy, the response,
    // and the positive-interaction event share one transaction. Reads and
    // storage preparation stay above this boundary, so no nested pool checkout
    // is possible while the roster connection is retained.
    final_policy::finish_challenge_response(
        st.pg(),
        &st.events,
        &user,
        final_policy::ChallengeResponseScope::new(
            id,
            ctx.participation.team_id,
            ctx.participation.id,
            challenge_id,
        ),
        response_grant,
        model,
    )
    .await
}
