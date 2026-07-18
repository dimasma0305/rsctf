//! Cheat/suspicion reports + anti-cheat block listing/clearing.

use super::*;

// ─── Cheat reports ─────────────────────────────────────────────────────────────

/// RSCTF `ParticipationModel` — team-participation reference embedded in a cheat
/// report.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParticipationModel {
    pub id: i32,
    pub team: Option<TeamModel>,
    pub status: ParticipationStatus,
    pub division: Option<String>,
    pub division_id: Option<i32>,
}

/// RSCTF `CheatInfoModel` — one recorded cheat/suspicion event. Wire shape matches
/// Api.ts `CheatInfoModel`: `ownedTeam` (flag owner), `submitTeam` (offender), and
/// the full offending `submission` (answer/status/time/user/team/challenge).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheatInfoModel {
    pub owned_team: Option<ParticipationModel>,
    pub submit_team: Option<ParticipationModel>,
    pub submission: Option<crate::controllers::game::SubmissionModel>,
}

/// Materialise a `ParticipationModel` (team + division) for a participation id.
async fn cheat_participation(
    st: &SharedState,
    participation_id: i32,
) -> AppResult<Option<ParticipationModel>> {
    let Some(p) = participation::Entity::find_by_id(participation_id)
        .one(&st.db)
        .await?
    else {
        return Ok(None);
    };

    let team = team::Entity::find_by_id(p.team_id)
        .one(&st.db)
        .await?
        .map(|t| TeamModel {
            id: t.id,
            name: t.name.clone(),
            avatar: t.avatar_url(),
        });

    let division = match p.division_id {
        Some(did) => division::Entity::find_by_id(did)
            .one(&st.db)
            .await?
            .map(|d| d.name),
        None => None,
    };

    Ok(Some(ParticipationModel {
        id: p.id,
        team,
        status: p.status,
        division,
        division_id: p.division_id,
    }))
}

/// `GET /api/admin/cheat-reports` — recent cheat/suspicion events (raw array),
/// newest first, mapped into RSCTF's `CheatInfoModel` shape from the
/// `SuspicionEvents` table.
pub async fn cheat_reports(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<ListQuery>,
) -> AppResult<RequestResponse<Vec<CheatInfoModel>>> {
    let count = q.count.clamp(1, 1000);
    let events = suspicion_event::Entity::find()
        .order_by_desc(suspicion_event::Column::CreatedAt)
        .offset(q.skip)
        .limit(count)
        .all(&st.db)
        .await?;

    let mut data = Vec::with_capacity(events.len());
    for e in events {
        // The flagged (offending) participation is `submitTeam`; the team whose
        // per-team flag was shared is `ownedTeam`, reconstructed alongside the full
        // offending submission from the event's challenge — the same flag-sharing
        // join `game::cheat` performs, keyed off the persisted event.
        let submit_team = cheat_participation(&st, e.participation_id).await?;
        let (owned_team, submission) = resolve_offender(&st, &e).await?;
        data.push(CheatInfoModel {
            owned_team,
            submit_team,
            submission,
        });
    }

    Ok(RequestResponse::ok(data))
}

/// Reconstruct the flag-owner participation (`ownedTeam`) and the full offending
/// `Submission` for a suspicion event, mirroring the flag-sharing detection in
/// `game::cheat`. Returns `(None, None)` for a non-submission event (e.g. a
/// fingerprint correlation with no `challenge_id`, or a challenge with no matching
/// submission on record).
async fn resolve_offender(
    st: &SharedState,
    e: &suspicion_event::Model,
) -> AppResult<(
    Option<ParticipationModel>,
    Option<crate::controllers::game::SubmissionModel>,
)> {
    let Some(cid) = e.challenge_id else {
        return Ok((None, None));
    };

    // This participation's submissions on the flagged challenge, newest first.
    let subs = submission::Entity::find()
        .filter(submission::Column::GameId.eq(e.game_id))
        .filter(submission::Column::ParticipationId.eq(e.participation_id))
        .filter(submission::Column::ChallengeId.eq(cid))
        .order_by_desc(submission::Column::SubmitTimeUtc)
        .all(&st.db)
        .await?;
    if subs.is_empty() {
        return Ok((None, None));
    }

    // (flag string) -> owning participation, from OTHER teams' live instances of
    // this challenge — the owner map `game::cheat` builds to spot a shared flag.
    let instances = game_instance::Entity::find()
        .filter(game_instance::Column::ChallengeId.eq(cid))
        .filter(game_instance::Column::ParticipationId.ne(e.participation_id))
        .all(&st.db)
        .await?;
    let flag_ids: Vec<i32> = instances.iter().filter_map(|i| i.flag_id).collect();
    let flag_of: std::collections::HashMap<i32, String> = if flag_ids.is_empty() {
        std::collections::HashMap::new()
    } else {
        flag_context::Entity::find()
            .filter(flag_context::Column::Id.is_in(flag_ids))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|f| (f.id, f.flag))
            .collect()
    };
    let mut owner_by_flag: std::collections::HashMap<String, i32> =
        std::collections::HashMap::new();
    for inst in &instances {
        if let Some(fid) = inst.flag_id {
            if let Some(flag) = flag_of.get(&fid) {
                owner_by_flag
                    .entry(flag.clone())
                    .or_insert(inst.participation_id);
            }
        }
    }

    // Submission-backed evidence points to the exact immutable attempt. Legacy
    // and aggregate rows retain the established best-match fallback.
    let evidence_submission_id = e
        .evidence_key
        .strip_prefix("submission:")
        .and_then(|id| id.parse::<i32>().ok());
    let (chosen, owner_pid) = evidence_submission_id
        .and_then(|id| subs.iter().find(|submission| submission.id == id))
        .map(|submission| (submission, owner_by_flag.get(&submission.answer).copied()))
        .or_else(|| {
            subs.iter().find_map(|submission| {
                owner_by_flag
                    .get(&submission.answer)
                    .map(|&owner| (submission, Some(owner)))
            })
        })
        .unwrap_or((&subs[0], None));

    let owned_team = match owner_pid {
        Some(pid) => cheat_participation(st, pid).await?,
        None => None,
    };

    // Full Submission model (answer/status/time/user/team/challenge).
    let user_name = match chosen.user_id {
        Some(uid) => user::Entity::find_by_id(uid)
            .one(&st.db)
            .await?
            .and_then(|u| u.user_name),
        None => None,
    };
    let team_name = team::Entity::find_by_id(chosen.team_id)
        .one(&st.db)
        .await?
        .map(|t| t.name);
    let challenge = game_challenge::Entity::find_by_id(cid)
        .one(&st.db)
        .await?
        .map(|c| c.title);

    let submission = crate::controllers::game::SubmissionModel {
        answer: chosen.answer.clone(),
        status: chosen.status,
        time: chosen.submit_time_utc,
        user: user_name,
        team: team_name,
        challenge,
    };

    Ok((owned_team, Some(submission)))
}

// ─── Anti-cheat blocks ─────────────────────────────────────────────────────────

/// `AntiCheatBlockModel` — one recorded anti-cheat conflict (RSCTF wire model).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AntiCheatBlockModel {
    pub id: i32,
    /// Blocked user's id (Uuid string).
    pub user_id: String,
    pub user_name: Option<String>,
    /// The account that owns the conflicting value (Uuid string), if known.
    pub conflict_user_id: Option<String>,
    pub conflict_user_name: Option<String>,
    /// `"Ip"` | `"Fingerprint"`.
    pub kind: String,
    pub conflicting_value: Option<String>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub occurred_at_utc: DateTime<Utc>,
}

/// Anti-cheat block listing query (`?count=`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AntiCheatBlocksQuery {
    #[serde(default = "default_count")]
    pub count: u64,
}

impl From<anti_cheat_block::Model> for AntiCheatBlockModel {
    fn from(m: anti_cheat_block::Model) -> Self {
        AntiCheatBlockModel {
            id: m.id,
            user_id: m.user_id.to_string(),
            user_name: m.user_name,
            conflict_user_id: m.conflict_user_id.map(|u| u.to_string()),
            conflict_user_name: m.conflict_user_name,
            kind: m.kind,
            conflicting_value: m.conflicting_value,
            occurred_at_utc: m.occurred_at_utc,
        }
    }
}

/// `GET /api/admin/anticheatblocks?count=` — recorded conflicts, newest-first.
pub async fn list_anti_cheat_blocks(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(q): Query<AntiCheatBlocksQuery>,
) -> AppResult<RequestResponse<Vec<AntiCheatBlockModel>>> {
    let rows = anti_cheat_block::Entity::find()
        .order_by_desc(anti_cheat_block::Column::OccurredAtUtc)
        .order_by_desc(anti_cheat_block::Column::Id)
        .limit(q.count)
        .all(&st.db)
        .await?;
    Ok(RequestResponse::ok(
        rows.into_iter().map(Into::into).collect(),
    ))
}

/// `DELETE /api/admin/anticheatblocks/{id}` — clear a recorded conflict.
pub async fn delete_anti_cheat_block(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
) -> AppResult<MessageResponse> {
    anti_cheat_block::Entity::delete_by_id(id)
        .exec(&st.db)
        .await?;
    Ok(MessageResponse::ok(""))
}
