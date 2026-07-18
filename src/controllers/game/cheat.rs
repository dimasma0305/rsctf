//! Cheat detection: flag-sharing reconstruction + collusion (RSI) reporting.
use super::*;

// ---------------------------------------------------------------------------
// Cheat detection — `cheatinfo` reconstructs flag-sharing incidents live; the
// aggregate `cheatreport`/`compare` pipelines remain typed TODO responses.
// ---------------------------------------------------------------------------

/// `GET /api/game/{id}/cheatinfo` — requires Monitor.
///
/// RSCTF persists a `CheatInfo` row whenever `GameInstanceRepository.CheckCheat`
/// finds a submission whose answer equals *another* team's per-team dynamic flag.
/// rsctf's inline judge doesn't run that check at submit time, so we reconstruct
/// the same incidents on read: build the `(challengeId, flag) -> owner`
/// map from every participation's `GameInstance.FlagContext`, then flag any
/// submission whose `(challengeId, answer)` lands on a flag owned by a *different*
/// participation. Recorded `suspicion_event` rows are also consulted so a flagged
/// participation's incident is still surfaced (deduplicated by submission id).
pub async fn cheat_info(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<Vec<CheatInfoModel>>> {
    let _ = load_game(&st, id).await?;
    let results = collect_cheat_incidents(&st, id).await?;
    Ok(RequestResponse::ok(results))
}

/// Reconstruct flag-sharing incidents for a game — shared by `cheatinfo` (raw
/// list) and `cheatreport` (grouped into collusion groups). Detection strategy is
/// documented on [`cheat_info`].
async fn collect_cheat_incidents(st: &SharedState, id: i32) -> AppResult<Vec<CheatInfoModel>> {
    // Participations in this game (submitter/owner resolution).
    let parts = participation::Entity::find()
        .filter(participation::Column::GameId.eq(id))
        .all(&st.db)
        .await?;
    let part_by_id: HashMap<i32, participation::Model> =
        parts.iter().map(|p| (p.id, p.clone())).collect();

    // Team + division lookups for the embedded ParticipationModel.
    let team_ids: Vec<i32> = parts.iter().map(|p| p.team_id).collect();
    let teams: HashMap<i32, team::Model> = if team_ids.is_empty() {
        HashMap::new()
    } else {
        team::Entity::find()
            .filter(team::Column::Id.is_in(team_ids))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|t| (t.id, t))
            .collect()
    };
    let division_names: HashMap<i32, String> = division::Entity::find()
        .filter(division::Column::GameId.eq(id))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|d| (d.id, d.name))
        .collect();

    // Per-team dynamic flags: (challengeId, flag) -> owning participation id.
    let part_ids: Vec<i32> = parts.iter().map(|p| p.id).collect();
    let instances = if part_ids.is_empty() {
        Vec::new()
    } else {
        game_instance::Entity::find()
            .filter(game_instance::Column::ParticipationId.is_in(part_ids))
            .all(&st.db)
            .await?
    };
    let flag_ids: Vec<i32> = instances.iter().filter_map(|i| i.flag_id).collect();
    let flag_of: HashMap<i32, String> = if flag_ids.is_empty() {
        HashMap::new()
    } else {
        flag_context::Entity::find()
            .filter(flag_context::Column::Id.is_in(flag_ids))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|f| (f.id, f.flag))
            .collect()
    };
    let mut owner: HashMap<(i32, String), i32> = HashMap::new();
    for inst in &instances {
        if let Some(fid) = inst.flag_id {
            if let Some(flag) = flag_of.get(&fid) {
                owner.insert((inst.challenge_id, flag.clone()), inst.participation_id);
            }
        }
    }

    // Every submission in the game — a stolen flag is graded WrongAnswer against
    // the thief's own instance, so we must scan all statuses, not just Accepted.
    let subs = submission::Entity::find()
        .filter(submission::Column::GameId.eq(id))
        .order_by_desc(submission::Column::SubmitTimeUtc)
        .all(&st.db)
        .await?;
    let challenge_titles = challenge_title_map(st, subs.iter().map(|s| s.challenge_id)).await?;
    let user_names = user_name_map(st, subs.iter().filter_map(|s| s.user_id)).await?;

    // Build a ParticipationModel for a participation id (owner or submitter).
    let make_part = |pid: i32| -> Option<ParticipationModel> {
        let p = part_by_id.get(&pid)?;
        let t = teams.get(&p.team_id);
        Some(ParticipationModel {
            id: p.id,
            team: TeamModel {
                id: p.team_id,
                name: t.map(|t| t.name.clone()),
                avatar: t.and_then(|t| t.avatar_url()),
            },
            status: p.status,
            division: p.division_id.and_then(|d| division_names.get(&d).cloned()),
            division_id: p.division_id,
        })
    };
    let make_sub = |s: &submission::Model| SubmissionModel {
        answer: s.answer.clone(),
        status: s.status,
        time: s.submit_time_utc,
        user: s.user_id.and_then(|u| user_names.get(&u).cloned()),
        team: teams.get(&s.team_id).map(|t| t.name.clone()),
        challenge: challenge_titles.get(&s.challenge_id).cloned(),
    };
    let build = |s: &submission::Model, owner_pid: i32| -> Option<CheatInfoModel> {
        Some(CheatInfoModel {
            owned_team: make_part(owner_pid)?,
            submit_team: make_part(s.participation_id)?,
            submission: make_sub(s),
        })
    };

    let mut results: Vec<CheatInfoModel> = Vec::new();
    let mut seen: HashSet<i32> = HashSet::new();

    // Primary detection: flag-sharing scan over every submission.
    for s in &subs {
        if let Some(&owner_pid) = owner.get(&(s.challenge_id, s.answer.clone())) {
            if owner_pid != s.participation_id {
                if let Some(info) = build(s, owner_pid) {
                    results.push(info);
                    seen.insert(s.id);
                }
            }
        }
    }

    // Union with recorded suspicion events: surface any still-resolvable incident
    // for a flagged participation not already caught by the live scan.
    let events = suspicion_event::Entity::find()
        .filter(suspicion_event::Column::GameId.eq(id))
        .all(&st.db)
        .await?;
    for ev in &events {
        let Some(cid) = ev.challenge_id else {
            continue;
        };
        for s in subs
            .iter()
            .filter(|s| s.participation_id == ev.participation_id && s.challenge_id == cid)
        {
            if seen.contains(&s.id) {
                continue;
            }
            if let Some(&owner_pid) = owner.get(&(s.challenge_id, s.answer.clone())) {
                if owner_pid != s.participation_id {
                    if let Some(info) = build(s, owner_pid) {
                        results.push(info);
                        seen.insert(s.id);
                    }
                }
            }
        }
    }

    Ok(results)
}

/// Query for the collusion `compare` endpoint (`?participationA=&participationB=`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompareQuery {
    pub participation_a: i32,
    pub participation_b: i32,
}

/// `GET /api/game/{id}/cheatreport` — requires Monitor.
///
/// Aggregates the reconstructed flag-sharing incidents (see `cheat_info`) into
/// the report shape: incidents are grouped by the unordered {ownerTeam,
/// submitTeam} pair, and each group is scored with the same RSI-style
/// solved-challenge overlap the `compare` endpoint uses.
///
/// Ported from RSCTF `CheatReportController.Get`, adapted to rsctf's data model:
/// - `suspicionList` — the persisted `suspicion_event` rows for the game,
///   grouped by participation and passed through the tiered fair-scoring
///   [`compute_breakdown`] (total score + risk band + per-event tier/counted).
/// - `identityOverlaps` / `ipAnalysis` — cross-team fingerprint/IP correlation
///   reconstructed from the `anti_cheat_block` conflict rows (rsctf has no
///   per-login Logs-with-fingerprint table; blocks are the equivalent source).
/// - `abnormalSolves` — left `[]`: the download/container game-event pipeline
///   those checks need isn't ported (the client tolerates the empty array).
pub async fn cheat_report(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<CheatReport>> {
    let _ = load_game(&st, id).await?;

    // Run the cheat-detection sweeps BEFORE building the suspicion list, so the
    // events they persist appear on this same request. A partial report is
    // unsafe for organizer decisions, so propagate a sweep failure while
    // preserving the successful response wire shape.
    crate::services::suspicion::run_abnormal_solve_checks(&st, id).await?;
    crate::services::suspicion::run_statistical_checks(&st, id).await?;
    crate::services::suspicion::run_correlation_checks(&st.db, id).await?;
    crate::services::suspicion::run_container_access_checks(&st, id).await?;
    crate::services::suspicion::run_honeypot_chain_checks(&st, id).await?;

    let incidents = collect_cheat_incidents(&st, id).await?;

    // Accepted submissions grouped by participation (ascending time) for RSI.
    let subs = submission::Entity::find()
        .filter(submission::Column::GameId.eq(id))
        .filter(submission::Column::Status.eq(AnswerResult::Accepted))
        .order_by_asc(submission::Column::SubmitTimeUtc)
        .all(&st.db)
        .await?;
    let mut by_part: HashMap<i32, Vec<submission::Model>> = HashMap::new();
    for s in subs {
        by_part.entry(s.participation_id).or_default().push(s);
    }
    let titles = game_challenge_titles(&st, id).await?;

    // Group incidents by the unordered participation pair; the lower participation
    // id is the first team so the grouping and team order are deterministic.
    type TeamRef = (i32, i32, String); // (participation_id, team_id, team_name)
    let mut pairs: BTreeMap<(i32, i32), (TeamRef, TeamRef)> = BTreeMap::new();
    for inc in &incidents {
        let name_of =
            |p: &ParticipationModel| p.team.name.clone().unwrap_or_else(|| "Unknown".to_string());
        let a: TeamRef = (
            inc.owned_team.id,
            inc.owned_team.team.id,
            name_of(&inc.owned_team),
        );
        let b: TeamRef = (
            inc.submit_team.id,
            inc.submit_team.team.id,
            name_of(&inc.submit_team),
        );
        let (first, second) = if a.0 <= b.0 { (a, b) } else { (b, a) };
        pairs.entry((first.0, second.0)).or_insert((first, second));
    }

    let empty: Vec<submission::Model> = Vec::new();
    let mut collusion_groups: Vec<Json> = Vec::new();
    for (first, second) in pairs.into_values() {
        let sub_a = by_part.get(&first.0).unwrap_or(&empty);
        let sub_b = by_part.get(&second.0).unwrap_or(&empty);
        let (rsi, common, detailed) = collusion_metrics(sub_a, sub_b, &titles);
        let details = format!(
            "Flag-sharing detected between team '{}' and team '{}': {} common solved challenge(s), {:.1}% solve-sequence similarity.",
            first.2,
            second.2,
            common.len(),
            rsi * 100.0
        );
        collusion_groups.push(serde_json::json!({
            "teams": [
                { "id": first.1, "name": first.2, "participationId": first.0 },
                { "id": second.1, "name": second.2, "participationId": second.0 },
            ],
            "averageRsi": rsi,
            "commonSolves": common,
            "details": details,
            "detailedSolves": detailed,
        }));
    }
    // Highest-similarity pairs first.
    collusion_groups.sort_by(|a, b| {
        let ra = a["averageRsi"].as_f64().unwrap_or(0.0);
        let rb = b["averageRsi"].as_f64().unwrap_or(0.0);
        rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
    });

    let suspicion_list = build_suspicion_list(&st, id).await?;
    let (ip_analysis, identity_overlaps) = build_identity_analysis(&st, id).await?;

    Ok(RequestResponse::ok(CheatReport {
        generated_at: Utc::now(),
        collusion_groups,
        suspicion_list,
        ip_analysis,
        identity_overlaps,
        // Download/container game-event pipeline not ported; see the doc comment.
        abnormal_solves: Vec::new(),
    }))
}

// ---------------------------------------------------------------------------
// Suspicion list — tiered fair-scoring aggregation of persisted events.
// ---------------------------------------------------------------------------

/// The `SuspicionEvents.tier` string the React client keys on
/// (`TIER_META` in `CheatInfo.tsx`): `hard | strong | behavioral | context`.
fn tier_key(tier: crate::services::suspicion::SuspicionTier) -> &'static str {
    use crate::services::suspicion::SuspicionTier::*;
    match tier {
        Hard => "hard",
        Strong => "strong",
        Context => "context",
        Behavioral => "behavioral",
    }
}

/// Build `CheatReport.suspicionList` — mirrors RSCTF `CheatReportController.Get`'s
/// suspicion-list block. Every participation in the game that has at least one
/// `suspicion_event` is scored through [`compute_breakdown`] (so identity/context
/// signals can never rank a team above one with hard evidence), then the roster is
/// ranked band-first. Returns `SuspicionRecordResult`-shaped JSON rows.
async fn build_suspicion_list(st: &SharedState, game_id: i32) -> AppResult<Vec<Json>> {
    use crate::models::data::suspicion_rule;
    use crate::services::suspicion::{
        compute_breakdown, default_weight, RiskBand, SuspicionEventRow, SuspicionType,
    };

    // Live per-rule weights (admin overrides) → compiled-in defaults.
    let weights: HashMap<String, i32> = suspicion_rule::Entity::find()
        .all(&st.db)
        .await?
        .into_iter()
        .map(|r| (r.rule_code, r.weight))
        .collect();
    // All suspicion events for the game, grouped by participation.
    let events = suspicion_event::Entity::find()
        .filter(suspicion_event::Column::GameId.eq(game_id))
        .all(&st.db)
        .await?;
    if events.is_empty() {
        return Ok(Vec::new());
    }
    let mut by_part: HashMap<i32, Vec<suspicion_event::Model>> = HashMap::new();
    for e in events {
        by_part.entry(e.participation_id).or_default().push(e);
    }

    // Team + status per participation.
    let part_ids: Vec<i32> = by_part.keys().copied().collect();
    let parts = participation::Entity::find()
        .filter(participation::Column::Id.is_in(part_ids))
        .all(&st.db)
        .await?;
    let part_by_id: HashMap<i32, participation::Model> =
        parts.iter().map(|p| (p.id, p.clone())).collect();
    let team_ids: Vec<i32> = parts.iter().map(|p| p.team_id).collect();
    let team_names: HashMap<i32, String> = if team_ids.is_empty() {
        HashMap::new()
    } else {
        team::Entity::find()
            .filter(team::Column::Id.is_in(team_ids))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|t| (t.id, t.name))
            .collect()
    };

    // (band, score, counted-incidents, teamId, row) — sort keys travel alongside.
    let mut rows: Vec<(RiskBand, i32, usize, i32, Json)> = Vec::new();
    for (pid, evs) in &by_part {
        let Some(p) = part_by_id.get(pid) else {
            continue;
        };

        // Each persisted event is one immutable incident. New events retain the
        // weight resolved at write time; legacy events fall back to the current
        // rule weight because they predate score-delta persistence.
        let event_rows: Vec<SuspicionEventRow> = evs
            .iter()
            .filter_map(|e| {
                let ty = SuspicionType::from_kind(e.kind)?;
                let code = ty.code();
                Some(SuspicionEventRow {
                    rule_code: code.to_string(),
                    evidence_key: e.evidence_key.clone(),
                    details: ty.default_entry().1.to_string(),
                    time: e.created_at,
                    score_delta: e.score_delta,
                })
            })
            .collect();
        if event_rows.is_empty() {
            continue;
        }

        let bd = compute_breakdown(&event_rows, |code: &str| {
            weights
                .get(code)
                .copied()
                .unwrap_or_else(|| default_weight(code))
        });

        // Events newest-first (matches RSCTF's `OrderByDescending(e => e.Time)`).
        let mut scored = bd.events.clone();
        scored.sort_by_key(|event| std::cmp::Reverse(event.time));
        let events_json: Vec<Json> = scored
            .iter()
            .map(|e| {
                serde_json::json!({
                    "type": e.rule_code,
                    "scoreDelta": e.score_delta,
                    "details": e.details,
                    "time": e.time.timestamp_millis(),
                    "tier": tier_key(e.tier),
                    "counted": e.counted,
                })
            })
            .collect();

        let team_name = team_names
            .get(&p.team_id)
            .cloned()
            .unwrap_or_else(|| "Unknown".to_string());
        let counted = bd.events.iter().filter(|e| e.counted).count();
        let record = serde_json::json!({
            "teamId": p.team_id,
            "participationId": p.id,
            "teamName": team_name,
            "score": bd.total,
            "band": bd.band.band_key(),
            "hard": bd.hard,
            "strong": bd.strong,
            "behavioral": bd.behavioral,
            "corroboration": bd.corroboration,
            "status": p.status,
            "events": events_json,
        });
        rows.push((bd.band, bd.total, counted, p.team_id, record));
    }

    // Rank: band desc (hard evidence on top), score desc, counted incidents desc,
    // teamId asc — the deterministic order RSCTF applies.
    rows.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.cmp(&a.1))
            .then(b.2.cmp(&a.2))
            .then(a.3.cmp(&b.3))
    });
    Ok(rows.into_iter().map(|r| r.4).collect())
}

// ---------------------------------------------------------------------------
// Identity / IP analysis — cross-team fingerprint & IP correlation.
// ---------------------------------------------------------------------------

/// Build `CheatReport.ipAnalysis` + `CheatReport.identityOverlaps` from the
/// `anti_cheat_block` conflict rows — the rsctf equivalent of RSCTF's
/// Logs-with-fingerprint correlation (rsctf has no per-login fingerprint column;
/// blocks record the same IP/fingerprint collisions).
///
/// Blocks are global (keyed on user UUIDs, no game scope), so each conflicting
/// account is mapped through `team_member` → a team **participating in this
/// game**; a shared value touching 2+ distinct in-game teams becomes:
/// - one `IdentityOverlapResult` (the summary row), and
/// - one `IpAnalysisResult` per involved team (`SharedIP` / `SharedFingerprint`).
///
/// RSCTF's context-gating (hide Context-tier rows unless the team also has a
/// behavioral+ signal) is intentionally NOT applied here: this port surfaces no
/// non-context ipAnalysis rows, so gating would blank the panel — the whole point
/// of the view is to surface these correlations for human review.
async fn build_identity_analysis(
    st: &SharedState,
    game_id: i32,
) -> AppResult<(Vec<Json>, Vec<Json>)> {
    use crate::models::data::{anti_cheat_block, team_member};
    use std::collections::{BTreeMap, BTreeSet};

    // Teams participating in this game.
    let parts = participation::Entity::find()
        .filter(participation::Column::GameId.eq(game_id))
        .all(&st.db)
        .await?;
    if parts.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let game_team_ids: Vec<i32> = {
        let set: BTreeSet<i32> = parts.iter().map(|p| p.team_id).collect();
        set.into_iter().collect()
    };
    let team_names: HashMap<i32, String> = team::Entity::find()
        .filter(team::Column::Id.is_in(game_team_ids.clone()))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|t| (t.id, t.name))
        .collect();
    let name_of = |tid: i32| {
        team_names
            .get(&tid)
            .cloned()
            .unwrap_or_else(|| "Unknown".to_string())
    };

    // user (UUID) → in-game team.
    let user_team: HashMap<Uuid, i32> = team_member::Entity::find()
        .filter(team_member::Column::TeamId.is_in(game_team_ids))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|m| (m.user_id, m.team_id))
        .collect();

    // Group anti-cheat blocks by (kind, shared value), collecting the in-game
    // teams (and their usernames) that touched the value.
    struct Group {
        team_users: BTreeMap<i32, BTreeSet<String>>,
        latest: DateTime<Utc>,
    }
    let mut groups: BTreeMap<(String, String), Group> = BTreeMap::new();
    let blocks = anti_cheat_block::Entity::find().all(&st.db).await?;
    for b in &blocks {
        let Some(value) = b.conflicting_value.clone() else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        let kind = if b.kind.eq_ignore_ascii_case("fingerprint") {
            "fingerprint"
        } else {
            "ip"
        };
        let g = groups
            .entry((kind.to_string(), value))
            .or_insert_with(|| Group {
                team_users: BTreeMap::new(),
                latest: b.occurred_at_utc,
            });
        if b.occurred_at_utc > g.latest {
            g.latest = b.occurred_at_utc;
        }
        for (uid, uname) in [
            (Some(b.user_id), b.user_name.clone()),
            (b.conflict_user_id, b.conflict_user_name.clone()),
        ] {
            if let Some(uid) = uid {
                if let Some(&tid) = user_team.get(&uid) {
                    let entry = g.team_users.entry(tid).or_default();
                    if let Some(n) = uname {
                        if !n.is_empty() {
                            entry.insert(n);
                        }
                    }
                }
            }
        }
    }

    // (teamId, time, row) for ipAnalysis; (kindRank, teamCount, value, row) for overlaps.
    let mut ip_rows: Vec<(i32, DateTime<Utc>, Json)> = Vec::new();
    let mut overlap_rows: Vec<(u8, usize, String, Json)> = Vec::new();

    for ((kind, value), g) in &groups {
        if g.team_users.len() < 2 {
            continue;
        }
        let is_fp = kind.as_str() == "fingerprint";
        let ty_str = if is_fp {
            "SharedFingerprint"
        } else {
            "SharedIP"
        };
        let team_ids: Vec<i32> = g.team_users.keys().copied().collect();

        // Distinct usernames across the group, capped like RSCTF (12).
        let mut seen = BTreeSet::new();
        let all_users: Vec<String> = g
            .team_users
            .values()
            .flatten()
            .filter(|u| seen.insert((*u).clone()))
            .take(12)
            .cloned()
            .collect();

        // Identity overlap (summary). Fingerprints are masked for display.
        let masked = if is_fp && value.chars().count() > 12 {
            let prefix: String = value.chars().take(12).collect();
            format!("{prefix}\u{2026}")
        } else {
            value.clone()
        };
        let overlap_team_names: Vec<String> =
            team_ids.iter().take(12).map(|t| name_of(*t)).collect();
        overlap_rows.push((
            if is_fp { 0 } else { 1 },
            team_ids.len(),
            value.clone(),
            serde_json::json!({
                "kind": kind,
                "value": masked,
                "teamCount": team_ids.len(),
                "teamNames": overlap_team_names,
                "userNames": all_users,
            }),
        ));

        // ipAnalysis: one row per involved team.
        let label = if is_fp { "browser fingerprint" } else { "IP" };
        let field = if is_fp { "Fingerprint" } else { "IP" };
        for (tid, users) in &g.team_users {
            let related: Vec<String> = g
                .team_users
                .keys()
                .filter(|k| *k != tid)
                .map(|t| name_of(*t))
                .collect();
            let this_users: Vec<String> = users.iter().take(6).cloned().collect();
            let related_users: Vec<String> = g
                .team_users
                .iter()
                .filter(|(k, _)| *k != tid)
                .flat_map(|(_, u)| u.iter().cloned())
                .take(8)
                .collect();
            let details = format!(
                "Summary: Same {label} observed across multiple teams\nTarget: team '{}'\n{field}: {value}\nSource teams: {}",
                name_of(*tid),
                related
                    .iter()
                    .map(|t| format!("team '{t}'"))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            ip_rows.push((
                *tid,
                g.latest,
                serde_json::json!({
                    "teamId": tid,
                    "teamName": name_of(*tid),
                    "type": ty_str,
                    "ip": value,
                    "time": g.latest.timestamp_millis(),
                    "details": details,
                    "relatedTeams": related,
                    "userNames": this_users,
                    "relatedUsers": related_users,
                }),
            ));
        }
    }

    // ipAnalysis: OrderBy(TeamId).ThenBy(Time).
    ip_rows.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    // identityOverlaps: fingerprints first, fewer teams first, value ordinal; cap 200.
    overlap_rows.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    overlap_rows.truncate(200);

    Ok((
        ip_rows.into_iter().map(|r| r.2).collect(),
        overlap_rows.into_iter().map(|r| r.3).collect(),
    ))
}

/// `GET /api/game/{id}/cheatreport/compare` — requires Monitor.
///
/// Mirrors RSCTF `CheatReportController.Compare`: for two participations in the
/// game, compute the RSI (`0.7·Jaccard(solved sets) + 0.3·LCS(solve order)`) and
/// the per-common-challenge solve-time detail rows.
pub async fn cheat_report_compare(
    State(st): State<SharedState>,
    _user: MonitorUser,
    Path(id): Path<i32>,
    Query(q): Query<CompareQuery>,
) -> AppResult<RequestResponse<CollusionCompareResult>> {
    let _ = load_game(&st, id).await?;

    if q.participation_a == q.participation_b {
        return Err(AppError::bad_request(
            "Cannot compare a participation with itself.",
        ));
    }

    // Both participations must exist and belong to this game.
    for pid in [q.participation_a, q.participation_b] {
        participation::Entity::find_by_id(pid)
            .one(&st.db)
            .await?
            .filter(|p| p.game_id == id)
            .ok_or_else(|| AppError::bad_request(format!("Participation {pid} not found.")))?;
    }

    let sub_a = accepted_subs_asc(&st, id, q.participation_a).await?;
    let sub_b = accepted_subs_asc(&st, id, q.participation_b).await?;
    let titles = game_challenge_titles(&st, id).await?;

    let (rsi, _common, details) = collusion_metrics(&sub_a, &sub_b, &titles);
    Ok(RequestResponse::ok(CollusionCompareResult { rsi, details }))
}

/// Accepted submissions for a participation in solve order (ascending time).
async fn accepted_subs_asc(
    st: &SharedState,
    game_id: i32,
    participation_id: i32,
) -> AppResult<Vec<submission::Model>> {
    Ok(submission::Entity::find()
        .filter(submission::Column::GameId.eq(game_id))
        .filter(submission::Column::ParticipationId.eq(participation_id))
        .filter(submission::Column::Status.eq(AnswerResult::Accepted))
        .order_by_asc(submission::Column::SubmitTimeUtc)
        .all(&st.db)
        .await?)
}

/// Challenge id -> title for every challenge in a game.
async fn game_challenge_titles(st: &SharedState, game_id: i32) -> AppResult<HashMap<i32, String>> {
    Ok(game_challenge::Entity::find()
        .filter(game_challenge::Column::GameId.eq(game_id))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|c| (c.id, c.title))
        .collect())
}

/// Length of the longest common subsequence of two challenge-id sequences
/// (mirrors RSCTF `GetLongestCommonSubsequence`, rolling one-row DP).
fn lcs_len(a: &[i32], b: &[i32]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let m = b.len();
    let mut dp = vec![0usize; m + 1];
    for &x in a {
        let mut prev = 0usize;
        for j in 0..m {
            let tmp = dp[j + 1];
            dp[j + 1] = if x == b[j] {
                prev + 1
            } else {
                dp[j + 1].max(dp[j])
            };
            prev = tmp;
        }
    }
    dp[m]
}

/// RSI + common-solve overlap between two participations' accepted submissions.
///
/// Returns `(rsi, commonSolveTitles, detailedSolves)` where `rsi =
/// 0.7·Jaccard(solved sets) + 0.3·(LCS(solve order)/min(len))`, mirroring RSCTF.
/// `detailedSolves` are `SequenceSuspectDetail`-shaped JSON rows, ordered by
/// solve-time gap (closest first), capped at 50, then by team-A solve time.
fn collusion_metrics(
    sub_a: &[submission::Model],
    sub_b: &[submission::Model],
    titles: &HashMap<i32, String>,
) -> (f64, Vec<String>, Vec<Json>) {
    let seq_a: Vec<i32> = sub_a.iter().map(|s| s.challenge_id).collect();
    let seq_b: Vec<i32> = sub_b.iter().map(|s| s.challenge_id).collect();
    let set_a: HashSet<i32> = seq_a.iter().copied().collect();
    let set_b: HashSet<i32> = seq_b.iter().copied().collect();

    let mut inter: Vec<i32> = set_a
        .iter()
        .copied()
        .filter(|c| set_b.contains(c))
        .collect();
    inter.sort_unstable();
    let union = set_a.union(&set_b).count();
    let jaccard = if union == 0 {
        0.0
    } else {
        inter.len() as f64 / union as f64
    };
    let lcs = lcs_len(&seq_a, &seq_b);
    let min_len = seq_a.len().min(seq_b.len());
    let lcs_score = if min_len == 0 {
        0.0
    } else {
        lcs as f64 / min_len as f64
    };
    let rsi = jaccard * 0.7 + lcs_score * 0.3;

    // Earliest accepted solve time per side (submissions are ascending, so the
    // first match is the earliest solve of that challenge).
    let mut rows: Vec<(String, DateTime<Utc>, DateTime<Utc>, f64)> = Vec::new();
    let mut common_solves: Vec<String> = Vec::new();
    for cid in &inter {
        let name = titles
            .get(cid)
            .cloned()
            .unwrap_or_else(|| "Unknown".to_string());
        common_solves.push(name.clone());
        let ta = sub_a
            .iter()
            .find(|s| s.challenge_id == *cid)
            .map(|s| s.submit_time_utc);
        let tb = sub_b
            .iter()
            .find(|s| s.challenge_id == *cid)
            .map(|s| s.submit_time_utc);
        if let (Some(ta), Some(tb)) = (ta, tb) {
            let diff = ((ta - tb).num_milliseconds().abs() as f64) / 1000.0;
            rows.push((name, ta, tb, diff));
        }
    }
    rows.sort_by(|a, b| a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal));
    rows.truncate(50);
    rows.sort_by_key(|row| row.1);

    let detailed: Vec<Json> = rows
        .into_iter()
        .map(|(name, ta, tb, diff)| {
            serde_json::json!({
                "challengeName": name,
                "timeA": ta.timestamp_millis(),
                "timeB": tb.timestamp_millis(),
                "timeDiff": diff,
            })
        })
        .collect();
    (rsi, common_solves, detailed)
}
