//! Evidence projections for the agent-artifact rules: the matched files with
//! their signature, snippet, and hash, and for a contradiction the team's own
//! "No AI used" declaration.

use super::*;

const MAX_ARTIFACT_ROWS: i64 = 20;

#[derive(Debug, sqlx::FromRow)]
struct ArtifactRow {
    id: i64,
    source: String,
    file_name: String,
    sha256: Vec<u8>,
    signature_key: String,
    signature_label: String,
    location: String,
    byte_offset: i64,
    snippet: String,
    uploaded_at: DateTime<Utc>,
    scanned_at: DateTime<Utc>,
    uploader: Option<String>,
    version: Option<i32>,
}

async fn artifact_rows(
    pool: &sqlx::PgPool,
    event: &EventEvidenceRow,
    source: Option<&str>,
    digest: Option<&str>,
) -> AppResult<Vec<ArtifactRow>> {
    sqlx::query_as::<_, ArtifactRow>(
        r#"SELECT artifact.id, artifact.source, artifact.file_name, artifact.sha256,
                  artifact.signature_key, artifact.signature_label, artifact.location,
                  artifact.byte_offset, artifact.snippet, artifact.uploaded_at,
                  artifact.scanned_at, account.user_name AS uploader,
                  upload.version
             FROM "AgentArtifactMatches" artifact
             LEFT JOIN "AspNetUsers" account ON account.id = artifact.uploaded_by
             LEFT JOIN "SolverUploads" upload ON upload.id = artifact.solver_upload_id
            WHERE artifact.game_id = $1 AND artifact.participation_id = $2
              AND ($3::text IS NULL OR artifact.source = $3)
              AND ($4::integer IS NULL OR artifact.challenge_id = $4)
              AND ($5::text IS NULL OR left(encode(artifact.sha256, 'hex'), 24) = $5)
            ORDER BY artifact.uploaded_at, artifact.id
            LIMIT $6"#,
    )
    .bind(event.game_id)
    .bind(event.participation_id)
    .bind(source)
    .bind(event.challenge_id)
    .bind(digest)
    .bind(MAX_ARTIFACT_ROWS)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

fn artifact_source(row: ArtifactRow) -> EvidenceSourceReview {
    let file = match (row.source.as_str(), row.version) {
        ("Solver", Some(version)) => format!("Solver upload v{version}: {}", row.file_name),
        ("Solver", None) => format!("Solver upload: {}", row.file_name),
        _ => format!("Writeup: {}", row.file_name),
    };
    let location = match row.location.as_str() {
        "Text" => "PDF page text",
        "Stream" => "Decompressed stream or archive member",
        _ => "File bytes",
    };
    EvidenceSourceReview {
        source_type: "agentArtifact".to_string(),
        title: format!("{} in {}", row.signature_label, row.file_name),
        source_id: Some(format!("artifact:{}", row.id)),
        recorded_at: Some(row.scanned_at),
        immutable: true,
        summary: "The submitted file contains text that matches an agent-artifact signature. The file was scanned as data and never executed.".to_string(),
        facts: vec![
            fact("File", file),
            fact("SHA-256", hex::encode(&row.sha256)),
            fact("Signature", format!("{} ({})", row.signature_label, row.signature_key)),
            fact("Found in", format!("{location}, offset {}", row.byte_offset)),
            fact("Matched text", row.snippet),
            fact("Uploaded", format_time(row.uploaded_at)),
            fact(
                "Uploaded by",
                row.uploader.unwrap_or_else(|| "not recorded".to_string()),
            ),
        ],
    }
}

#[derive(Debug, sqlx::FromRow)]
struct DeclarationRow {
    declared_no_ai: bool,
    updated_at: DateTime<Utc>,
    submitted_by: Option<String>,
    first_declared_at: Option<DateTime<Utc>>,
}

pub(super) async fn add_agent_artifact_source(
    pool: &sqlx::PgPool,
    event: &EventEvidenceRow,
    ty: SuspicionType,
    review: &mut SuspicionEvidenceReview,
) -> AppResult<()> {
    let rows = if ty == SuspicionType::AiDeclarationContradiction {
        let declaration = sqlx::query_as::<_, DeclarationRow>(
            r#"SELECT link.declared_no_ai, link.updated_at,
                      account.user_name AS submitted_by,
                      (SELECT MIN(history.occurred_at) FROM "AiChatLinkEvents" history
                        WHERE history.game_id = link.game_id
                          AND history.participation_id = link.participation_id
                          AND history.challenge_id = link.challenge_id
                          AND history.declared_no_ai) AS first_declared_at
                 FROM "AiChatLinks" link
                 LEFT JOIN "AspNetUsers" account ON account.id = link.submitted_by
                WHERE link.game_id = $1 AND link.participation_id = $2
                  AND link.challenge_id = $3"#,
        )
        .bind(event.game_id)
        .bind(event.participation_id)
        .bind(event.challenge_id)
        .fetch_optional(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if let Some(declaration) = declaration {
            review.sources.push(EvidenceSourceReview {
                source_type: "aiDisclosure".to_string(),
                title: "Team AI disclosure for this challenge".to_string(),
                source_id: Some("ai-chat-links".to_string()),
                recorded_at: Some(declaration.updated_at),
                immutable: false,
                summary: if declaration.declared_no_ai {
                    "The team currently declares that no AI was used for this challenge."
                } else {
                    "The team has since replaced its \"No AI used\" declaration; the edit history keeps the original."
                }
                .to_string(),
                facts: vec![
                    fact(
                        "Current declaration",
                        if declaration.declared_no_ai { "No AI used" } else { "AI chat links" },
                    ),
                    fact(
                        "First declared no AI",
                        declaration
                            .first_declared_at
                            .map(format_time)
                            .unwrap_or_else(|| "not in history".to_string()),
                    ),
                    fact(
                        "Last saved by",
                        declaration.submitted_by.unwrap_or_else(|| "unknown".to_string()),
                    ),
                ],
            });
        }
        artifact_rows(pool, event, Some("Solver"), None).await?
    } else {
        // agent-artifact:solver:{challenge}:{digest} or agent-artifact:writeup:{digest}
        let parts = event.evidence_key.split(':').collect::<Vec<_>>();
        let (source, digest) = match parts.as_slice() {
            ["agent-artifact", "solver", _, digest] => ("Solver", *digest),
            ["agent-artifact", "writeup", digest] => ("Writeup", *digest),
            _ => return Ok(()),
        };
        artifact_rows(pool, event, Some(source), Some(digest)).await?
    };
    for row in rows {
        review.sources.push(artifact_source(row));
    }
    if !review.sources.is_empty() {
        mark_supporting(review);
    }
    review.limitations.insert(
        0,
        "A trace shows which tool touched the file, not that the event's rules were broken. Check what this event allows before acting, and ask the team for an explanation."
            .to_string(),
    );
    Ok(())
}
