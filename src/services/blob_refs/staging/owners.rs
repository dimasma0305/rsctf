//! Durable-owner validation for published staging receipt replays.

use super::*;

fn parse_scoped_i32(scope: &str, prefix: &str) -> Option<i32> {
    scope.strip_prefix(prefix)?.parse().ok()
}

/// A published staging receipt proves that one reference was acquired in a
/// prior transaction; it does not by itself prove that its named domain owner
/// still points at that file. Revalidate every stable owner scope before an old
/// operation may replay, otherwise an obsolete avatar/poster/upload operation
/// could repoint an owner after a newer change merely because another owner
/// still keeps the content hash referenced.
pub(super) async fn published_owner_still_matches(
    transaction: &mut Transaction<'_, Postgres>,
    staged: &StagedBlob,
    publication_scope: &str,
    file_id: i32,
) -> AppResult<bool> {
    let matches = if publication_scope == "account-avatar" {
        let Some(user_id) = staged.owner_user_id else {
            return Ok(false);
        };
        sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(
                   SELECT 1 FROM "AspNetUsers"
                    WHERE id = $1 AND avatar_hash = $2
               )"#,
        )
        .bind(user_id)
        .bind(&staged.blob.hash)
        .fetch_one(&mut **transaction)
        .await
    } else if publication_scope == "platform-branding" {
        sqlx::query_scalar::<_, bool>(
            r#"SELECT COUNT(DISTINCT config_key) = 2
                 FROM "Configs"
                WHERE config_key IN ('GlobalConfig:LogoHash', 'GlobalConfig:FaviconHash')
                  AND value = $1"#,
        )
        .bind(&staged.blob.hash)
        .fetch_one(&mut **transaction)
        .await
    } else if let Some(team_id) = parse_scoped_i32(publication_scope, "team-avatar:") {
        sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(SELECT 1 FROM "Teams" WHERE id = $1 AND avatar_hash = $2)"#,
        )
        .bind(team_id)
        .bind(&staged.blob.hash)
        .fetch_one(&mut **transaction)
        .await
    } else if let Some(game_id) = parse_scoped_i32(publication_scope, "game-poster:") {
        sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(SELECT 1 FROM "Games" WHERE id = $1 AND poster_hash = $2)"#,
        )
        .bind(game_id)
        .bind(&staged.blob.hash)
        .fetch_one(&mut **transaction)
        .await
    } else if let Some(attachment_id) = parse_scoped_i32(publication_scope, "attachment:") {
        sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(
                   SELECT 1 FROM "Attachments"
                    WHERE id = $1 AND local_file_id = $2
               )"#,
        )
        .bind(attachment_id)
        .bind(file_id)
        .fetch_one(&mut **transaction)
        .await
    } else if let Some(challenge_id) = publication_scope
        .strip_prefix("challenge-archive:")
        .or_else(|| publication_scope.strip_prefix("challenge-source-archive:"))
        .and_then(|value| value.parse::<i32>().ok())
    {
        sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(
                   SELECT 1 FROM "GameChallenges"
                    WHERE id = $1 AND original_archive_blob_path = $2
               )"#,
        )
        .bind(challenge_id)
        .bind(&staged.blob.hash)
        .fetch_one(&mut **transaction)
        .await
    } else if let Some(challenge_id) = parse_scoped_i32(publication_scope, "challenge-attachment:")
    {
        sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(
                   SELECT 1
                     FROM "GameChallenges" challenge
                     JOIN "Attachments" attachment
                       ON attachment.id = challenge.attachment_id
                    WHERE challenge.id = $1 AND attachment.local_file_id = $2
               )"#,
        )
        .bind(challenge_id)
        .bind(file_id)
        .fetch_one(&mut **transaction)
        .await
    } else if let Some(service_id) = parse_scoped_i32(publication_scope, "ad-service-snapshot:") {
        sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(
                   SELECT 1 FROM "AdServiceSnapshots"
                    WHERE team_service_id = $1 AND local_file_id = $2
               )"#,
        )
        .bind(service_id)
        .bind(file_id)
        .fetch_one(&mut **transaction)
        .await
    } else if let Some(rest) = publication_scope.strip_prefix("writeup:") {
        let mut parts = rest.split(':');
        let parsed = parts
            .next()
            .and_then(|game| game.parse::<i32>().ok())
            .zip(
                parts
                    .next()
                    .and_then(|participation| participation.parse().ok()),
            )
            .filter(|_| parts.next().is_none());
        let Some((game_id, participation_id)) = parsed else {
            return Ok(false);
        };
        sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(
                   SELECT 1 FROM "Participations"
                    WHERE id = $1 AND game_id = $2 AND writeup_id = $3
               )"#,
        )
        .bind(participation_id)
        .bind(game_id)
        .bind(file_id)
        .fetch_one(&mut **transaction)
        .await
    } else {
        // Internal one-shot stages use random operation IDs and have no
        // externally replayable receipt. Keep their established semantics;
        // stable user/domain scopes above are always proven explicitly.
        return Ok(true);
    };
    matches.map_err(database_error)
}
