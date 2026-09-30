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

/// The team's "No AI used" history for one challenge, read from the
/// append-only disclosure events so a later clear or replacement cannot hide
/// it, plus the current disclosure for context.
#[derive(Debug, sqlx::FromRow)]
struct DeclarationRow {
    first_declared_at: Option<DateTime<Utc>>,
    first_declared_by: Option<String>,
    declarations: i64,
    current: Option<String>,
    current_saved_at: Option<DateTime<Utc>>,
}

const DECLARATION_SQL: &str = r#"
    SELECT first.occurred_at AS first_declared_at,
           first_account.user_name AS first_declared_by,
           (SELECT COUNT(*) FROM "AiChatLinkEvents" history
             WHERE history.game_id = $1 AND history.participation_id = $2
               AND history.challenge_id = $3 AND history.declared_no_ai) AS declarations,
           CASE WHEN link.participation_id IS NULL THEN 'Cleared'
                WHEN link.declared_no_ai THEN 'No AI used'
                ELSE 'AI chat links' END AS current,
           link.updated_at AS current_saved_at
      FROM (SELECT 1) anchor
      LEFT JOIN LATERAL (
          SELECT history.occurred_at, history.user_id
            FROM "AiChatLinkEvents" history
           WHERE history.game_id = $1 AND history.participation_id = $2
             AND history.challenge_id = $3 AND history.declared_no_ai
           ORDER BY history.occurred_at, history.id
           LIMIT 1
      ) first ON TRUE
      LEFT JOIN "AspNetUsers" first_account ON first_account.id = first.user_id
      LEFT JOIN "AiChatLinks" link
        ON link.game_id = $1 AND link.participation_id = $2 AND link.challenge_id = $3
"#;

pub(super) async fn add_agent_artifact_source(
    pool: &sqlx::PgPool,
    event: &EventEvidenceRow,
    ty: SuspicionType,
    review: &mut SuspicionEvidenceReview,
) -> AppResult<()> {
    let mut found = 0usize;
    let rows = if ty == SuspicionType::AiDeclarationContradiction {
        let declaration = sqlx::query_as::<_, DeclarationRow>(DECLARATION_SQL)
            .bind(event.game_id)
            .bind(event.participation_id)
            .bind(event.challenge_id)
            .fetch_one(pool)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        let current_declares = declaration.current.as_deref() == Some("No AI used");
        if declaration.first_declared_at.is_some() || current_declares {
            found += 1;
            let current = declaration.current.unwrap_or_else(|| "Cleared".to_string());
            review.sources.push(EvidenceSourceReview {
                source_type: "aiDisclosure".to_string(),
                title: "Team \"No AI used\" declaration for this challenge".to_string(),
                source_id: Some("ai-chat-link-events".to_string()),
                recorded_at: declaration.first_declared_at.or(declaration.current_saved_at),
                immutable: declaration.first_declared_at.is_some(),
                summary: if current_declares {
                    "The team declares that no AI was used for this challenge."
                } else {
                    "The team declared that no AI was used for this challenge and later changed or cleared that declaration; the append-only history keeps it."
                }
                .to_string(),
                facts: vec![
                    fact(
                        "First declared no AI",
                        declaration
                            .first_declared_at
                            .map(format_time)
                            .unwrap_or_else(|| "not in history".to_string()),
                    ),
                    fact(
                        "Declared by",
                        declaration
                            .first_declared_by
                            .unwrap_or_else(|| "unknown".to_string()),
                    ),
                    fact("Times declared", declaration.declarations.to_string()),
                    fact("Current disclosure", current),
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
    found += rows.len();
    for row in rows {
        review.sources.push(artifact_source(row));
    }
    // `base_review` always adds the event row itself; only real artifact or
    // declaration rows make this evidence supporting.
    if found > 0 {
        mark_supporting(review);
    }
    review.limitations.insert(
        0,
        "A trace shows which tool touched the file, not that the event's rules were broken. Check what this event allows before acting, and ask the team for an explanation."
            .to_string(),
    );
    Ok(())
}
