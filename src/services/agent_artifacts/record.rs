//! Scan submitted files, keep matches as immutable review evidence, and raise
//! the `AgentArtifact` and `AiDeclarationContradiction` suspicion rules.
//! Every write is idempotent on the file content and signature, so a rescan
//! never duplicates evidence or score.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{enabled_signatures, load_rows, scan_file, ArtifactHit, CompiledSignature};
use crate::app_state::SharedState;
use crate::services::suspicion::{record_agent_artifact_event, SuspicionType};
use crate::utils::error::{AppError, AppResult};

/// Upper bound for a writeup read back from storage for scanning.
const MAX_WRITEUP_SCAN_BYTES: usize = crate::utils::upload::WRITEUP_FILE_BYTES + 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Solver,
    Writeup,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Self::Solver => "Solver",
            Self::Writeup => "Writeup",
        }
    }
}

struct ScannedFile<'a> {
    game_id: i32,
    participation_id: i32,
    challenge_id: Option<i32>,
    source: Source,
    solver_upload_id: Option<i64>,
    file_name: &'a str,
    sha256: &'a [u8],
    uploaded_by: Option<Uuid>,
    uploaded_at: DateTime<Utc>,
}

fn db_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

async fn signatures(st: &SharedState) -> AppResult<Vec<CompiledSignature>> {
    Ok(enabled_signatures(&load_rows(st.pg()).await?))
}

async fn scan_blocking(
    signatures: Vec<CompiledSignature>,
    bytes: Vec<u8>,
) -> AppResult<Vec<ArtifactHit>> {
    tokio::task::spawn_blocking(move || scan_file(&signatures, &bytes))
        .await
        .map_err(|error| AppError::internal(format!("artifact scan task failed: {error}")))
}

async fn store_hits(
    st: &SharedState,
    file: &ScannedFile<'_>,
    hits: &[ArtifactHit],
) -> AppResult<()> {
    for hit in hits {
        sqlx::query(
            r#"INSERT INTO "AgentArtifactMatches"
                 (game_id, participation_id, challenge_id, source, solver_upload_id,
                  file_name, sha256, signature_key, signature_label, location,
                  byte_offset, snippet, uploaded_by, uploaded_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
               ON CONFLICT DO NOTHING"#,
        )
        .bind(file.game_id)
        .bind(file.participation_id)
        .bind(file.challenge_id)
        .bind(file.source.as_str())
        .bind(file.solver_upload_id)
        .bind(file.file_name.chars().take(255).collect::<String>())
        .bind(file.sha256)
        .bind(&hit.signature_key)
        .bind(hit.signature_label.chars().take(64).collect::<String>())
        .bind(hit.location.as_str())
        .bind(i64::try_from(hit.byte_offset).unwrap_or(i64::MAX))
        .bind(&hit.snippet)
        .bind(file.uploaded_by)
        .bind(file.uploaded_at)
        .execute(st.pg())
        .await
        .map_err(db_error)?;
    }
    Ok(())
}

fn artifact_evidence_key(file: &ScannedFile<'_>) -> String {
    let digest = hex::encode(&file.sha256[..12]);
    match (file.source, file.challenge_id) {
        (Source::Solver, Some(challenge_id)) => {
            format!("agent-artifact:solver:{challenge_id}:{digest}")
        }
        _ => format!("agent-artifact:writeup:{digest}"),
    }
}

/// Store the matches for one file and raise `AgentArtifact` when there is at
/// least one. Returns whether a new suspicion event was written.
async fn record_file(
    st: &SharedState,
    file: ScannedFile<'_>,
    hits: &[ArtifactHit],
) -> AppResult<bool> {
    if hits.is_empty() {
        return Ok(false);
    }
    store_hits(st, &file, hits).await?;
    record_agent_artifact_event(
        &st.db,
        file.game_id,
        file.participation_id,
        file.challenge_id,
        SuspicionType::AgentArtifact,
        &artifact_evidence_key(&file),
        file.uploaded_at,
    )
    .await
}

async fn competitive_game(st: &SharedState, game_id: i32) -> AppResult<bool> {
    let practice: Option<bool> =
        sqlx::query_scalar(r#"SELECT practice_mode FROM "Games" WHERE id = $1"#)
            .bind(game_id)
            .fetch_optional(st.pg())
            .await
            .map_err(db_error)?;
    Ok(practice == Some(false))
}

#[derive(sqlx::FromRow)]
struct SolverRow {
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
    file_name: String,
    sha256: Vec<u8>,
    content: Vec<u8>,
    uploaded_by: Option<Uuid>,
    uploaded_at: DateTime<Utc>,
}

async fn scan_solver_with(
    st: &SharedState,
    signatures: &[CompiledSignature],
    upload_id: i64,
) -> AppResult<bool> {
    let Some(row) = sqlx::query_as::<_, SolverRow>(
        r#"SELECT game_id, participation_id, challenge_id, file_name, sha256, content,
                  uploaded_by, uploaded_at
             FROM "SolverUploads" WHERE id = $1"#,
    )
    .bind(upload_id)
    .fetch_optional(st.pg())
    .await
    .map_err(db_error)?
    else {
        return Ok(false);
    };
    if !competitive_game(st, row.game_id).await? {
        return Ok(false);
    }
    let hits = scan_blocking(signatures.to_vec(), row.content).await?;
    let mut changed = record_file(
        st,
        ScannedFile {
            game_id: row.game_id,
            participation_id: row.participation_id,
            challenge_id: Some(row.challenge_id),
            source: Source::Solver,
            solver_upload_id: Some(upload_id),
            file_name: &row.file_name,
            sha256: &row.sha256,
            uploaded_by: row.uploaded_by,
            uploaded_at: row.uploaded_at,
        },
        &hits,
    )
    .await?;
    changed |=
        evaluate_contradiction(st, row.game_id, row.participation_id, row.challenge_id).await?;
    Ok(changed)
}

#[derive(sqlx::FromRow)]
struct WriteupRow {
    game_id: i32,
    name: String,
    hash: String,
    upload_time_utc: DateTime<Utc>,
}

async fn scan_writeup_with(
    st: &SharedState,
    signatures: &[CompiledSignature],
    participation_id: i32,
) -> AppResult<bool> {
    let Some(row) = sqlx::query_as::<_, WriteupRow>(
        r#"SELECT participation.game_id, file.name, file.hash, file.upload_time_utc
             FROM "Participations" participation
             JOIN "Files" file ON file.id = participation.writeup_id
            WHERE participation.id = $1"#,
    )
    .bind(participation_id)
    .fetch_optional(st.pg())
    .await
    .map_err(db_error)?
    else {
        return Ok(false);
    };
    if !competitive_game(st, row.game_id).await? {
        return Ok(false);
    }
    let bytes = st
        .storage
        .load_bounded(&row.hash, MAX_WRITEUP_SCAN_BYTES)
        .await?;
    let sha256 = Sha256::digest(&bytes).to_vec();
    let hits = scan_blocking(signatures.to_vec(), bytes).await?;
    record_file(
        st,
        ScannedFile {
            game_id: row.game_id,
            participation_id,
            challenge_id: None,
            source: Source::Writeup,
            solver_upload_id: None,
            file_name: &row.name,
            sha256: &sha256,
            uploaded_by: None,
            uploaded_at: row.upload_time_utc,
        },
        &hits,
    )
    .await
}

/// Scan one solver version. Returns whether new suspicion evidence was written.
pub async fn scan_solver_upload(st: &SharedState, upload_id: i64) -> AppResult<bool> {
    let signatures = signatures(st).await?;
    scan_solver_with(st, &signatures, upload_id).await
}

/// Scan a team's current writeup. Returns whether new suspicion evidence was written.
pub async fn scan_writeup(st: &SharedState, participation_id: i32) -> AppResult<bool> {
    let signatures = signatures(st).await?;
    scan_writeup_with(st, &signatures, participation_id).await
}

/// Raise `AiDeclarationContradiction` when the team declared "No AI used" for
/// a challenge whose own solver upload carries an agent artifact. Called after
/// a solver scan and after a "No AI used" declaration, so either order works.
pub async fn evaluate_contradiction(
    st: &SharedState,
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
) -> AppResult<bool> {
    let contradicted: bool = sqlx::query_scalar(
        r#"SELECT EXISTS (
               SELECT 1 FROM "AgentArtifactMatches" artifact
                WHERE artifact.game_id = $1 AND artifact.participation_id = $2
                  AND artifact.challenge_id = $3 AND artifact.source = 'Solver'
           ) AND EXISTS (
               SELECT 1 FROM "AiChatLinks" link
                WHERE link.game_id = $1 AND link.participation_id = $2
                  AND link.challenge_id = $3 AND link.declared_no_ai
           ) AND EXISTS (
               SELECT 1 FROM "Games" game WHERE game.id = $1 AND NOT game.practice_mode
           )"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .bind(challenge_id)
    .fetch_one(st.pg())
    .await
    .map_err(db_error)?;
    if !contradicted {
        return Ok(false);
    }
    record_agent_artifact_event(
        &st.db,
        game_id,
        participation_id,
        Some(challenge_id),
        SuspicionType::AiDeclarationContradiction,
        &format!("ai-declaration:{challenge_id}"),
        Utc::now(),
    )
    .await
}

/// Run a scan after an upload has committed. A scan failure is logged and
/// never fails or delays the upload itself.
pub fn spawn_solver_scan(st: SharedState, upload_id: i64) {
    tokio::spawn(async move {
        match scan_solver_upload(&st, upload_id).await {
            Ok(true) => {
                if let Ok(Some(game_id)) = sqlx::query_scalar::<_, i32>(
                    r#"SELECT game_id FROM "SolverUploads" WHERE id = $1"#,
                )
                .bind(upload_id)
                .fetch_optional(st.pg())
                .await
                {
                    crate::controllers::game::invalidate_cheat_report(&st, game_id).await;
                }
            }
            Ok(false) => {}
            Err(error) => tracing::warn!(%error, upload_id, "solver artifact scan failed"),
        }
    });
}

pub fn spawn_writeup_scan(st: SharedState, game_id: i32, participation_id: i32) {
    tokio::spawn(async move {
        match scan_writeup(&st, participation_id).await {
            Ok(true) => crate::controllers::game::invalidate_cheat_report(&st, game_id).await,
            Ok(false) => {}
            Err(error) => tracing::warn!(%error, participation_id, "writeup artifact scan failed"),
        }
    });
}

#[derive(Debug, Default, Serialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct RescanSummary {
    pub solvers_scanned: usize,
    pub writeups_scanned: usize,
    pub new_events: usize,
    pub failures: usize,
}

/// Rescan every solver version and current writeup of one event with the
/// current signature list. Sequential and idempotent; for administrators after
/// a signature change or for files uploaded before this detector existed.
pub async fn rescan_game(st: &SharedState, game_id: i32) -> AppResult<RescanSummary> {
    let signatures = signatures(st).await?;
    let mut summary = RescanSummary::default();
    let uploads: Vec<i64> =
        sqlx::query_scalar(r#"SELECT id FROM "SolverUploads" WHERE game_id = $1 ORDER BY id"#)
            .bind(game_id)
            .fetch_all(st.pg())
            .await
            .map_err(db_error)?;
    for upload_id in uploads {
        summary.solvers_scanned += 1;
        match scan_solver_with(st, &signatures, upload_id).await {
            Ok(changed) => summary.new_events += usize::from(changed),
            Err(error) => {
                summary.failures += 1;
                tracing::warn!(%error, upload_id, "solver artifact rescan failed");
            }
        }
    }
    let participations: Vec<i32> = sqlx::query_scalar(
        r#"SELECT id FROM "Participations"
            WHERE game_id = $1 AND writeup_id IS NOT NULL ORDER BY id"#,
    )
    .bind(game_id)
    .fetch_all(st.pg())
    .await
    .map_err(db_error)?;
    for participation_id in participations {
        summary.writeups_scanned += 1;
        match scan_writeup_with(st, &signatures, participation_id).await {
            Ok(changed) => summary.new_events += usize::from(changed),
            Err(error) => {
                summary.failures += 1;
                tracing::warn!(%error, participation_id, "writeup artifact rescan failed");
            }
        }
    }
    crate::controllers::game::invalidate_cheat_report(st, game_id).await;
    Ok(summary)
}
