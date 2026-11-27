//! Validate and register the original source distributed to hosted A&D teams.

use super::{ssh_sources::SshSource, PatchManifest};
use crate::utils::{
    enums::ChallengeType,
    error::{AppError, AppResult},
};
use sqlx::PgConnection;

pub(crate) struct Registration {
    pub(crate) tree_id: String,
    pub(crate) ssh_sources: Vec<SshSource>,
    pub(crate) data_mount: Option<String>,
}

impl Registration {
    pub(crate) fn from_zip(archive: &[u8]) -> Result<Self, String> {
        let manifest = PatchManifest::from_zip(archive)?;
        Ok(Self {
            tree_id: manifest.tree_id(),
            ssh_sources: manifest.ssh_sources(archive)?,
            data_mount: Self::data_mount(archive)?,
        })
    }

    fn data_mount(archive: &[u8]) -> Result<Option<String>, String> {
        use std::io::{Cursor, Read};
        let mut zip = zip::ZipArchive::new(Cursor::new(archive)).map_err(|e| e.to_string())?;
        let file = match zip.by_name(".rsctf-data-dir") {
            Ok(file) => file,
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let mut text = String::new();
        file.take(257)
            .read_to_string(&mut text)
            .map_err(|_| "Invalid .rsctf-data-dir")?;
        let path = text
            .strip_suffix("\r\n")
            .or_else(|| text.strip_suffix('\n'))
            .unwrap_or(&text);
        if !crate::services::container::application_data_path(path) {
            return Err(
                ".rsctf-data-dir must contain /data or one directory under /var/lib".into(),
            );
        }
        Ok(Some(path.to_owned()))
    }
}

/// Seed the organizer's starter before kickoff. An existing starter belongs to
/// the challenge editor, which alone may replace it with an expected tree ID.
pub(crate) async fn seed_from_repository(
    connection: &mut PgConnection,
    game_id: i32,
    challenge_id: i32,
    archive: &[u8],
    registration: &Registration,
) -> AppResult<()> {
    sqlx::query(
        r#"WITH hosted AS MATERIALIZED (
             SELECT c.id, g.start_time_utc
               FROM "Games" g JOIN "GameChallenges" c ON c.game_id=g.id
              WHERE g.id=$1 AND c.id=$2 AND c."Type"=$3
                AND NOT c.ad_self_hosted AND NOT c.deletion_pending AND NOT g.deletion_pending
              FOR SHARE OF g FOR UPDATE OF c
           )
           INSERT INTO "AdPatchStarters" (challenge_id,tree_id,archive,ssh_sources,data_mount)
           SELECT h.id,$4,$5,$6,$7 FROM hosted h
            WHERE h.start_time_utc > clock_timestamp()
              AND ($7::text IS NULL OR NOT EXISTS (
                   SELECT 1 FROM "AdTeamServices" WHERE challenge_id=h.id))
           ON CONFLICT (challenge_id) DO NOTHING"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(ChallengeType::AttackDefense as i16)
    .bind(&registration.tree_id)
    .bind(archive)
    .bind(sqlx::types::Json(&registration.ssh_sources))
    .bind(registration.data_mount.as_deref())
    .execute(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}
