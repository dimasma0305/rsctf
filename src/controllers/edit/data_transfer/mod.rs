//! Competition data archives: a complete, portable backup of one game's
//! definition plus everything recorded while it ran (roster, submissions,
//! events, evidence, every A&D/KotH record and rollup, rendered scoreboards),
//! and the admin-only restore that replays such an archive into a new hidden
//! game on any installation.
//!
//! Layout of `game-{id}-data.zip`:
//!
//! - `game.json`, `challenges/*.json`, `files/{hash}`: the definition export.
//! - `scoreboards/*.json`: the boards as monitors see them, for reading the
//!   backup without a platform. Ignored on restore.
//! - `data/<table>.jsonl`: one JSON object per row for every table in
//!   [`spec::catalog`], keys in camelCase, values as PostgreSQL emits them.
//! - `manifest.json`: kind, format version, source game, and row counts.

use super::*;

#[cfg(test)]
#[path = "db_tests.rs"]
mod db_tests;
mod export;
mod import;
mod roster;
mod spec;
#[cfg(test)]
mod tests;

pub use export::export_game_data;
pub use import::import_game_data;

pub(crate) const ARCHIVE_KIND: &str = "rsctf-competition-data";
pub(crate) const ARCHIVE_FORMAT_VERSION: u32 = 1;

/// `manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataArchiveManifest {
    pub kind: String,
    pub format_version: u32,
    #[serde(with = "crate::utils::datetime::millis")]
    pub exported_at_utc: DateTime<Utc>,
    pub source_game_id: i32,
    pub title: String,
    pub platform_version: String,
    /// False when the export was requested without attachment blobs; the
    /// attachment metadata is still present and a restore leaves them empty.
    #[serde(default = "default_true")]
    pub attachments_bundled: bool,
    pub tables: Vec<DataArchiveTable>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataArchiveTable {
    pub name: String,
    pub file: String,
    pub rows: u64,
}

/// `POST /api/edit/games/import/data` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameDataImportResult {
    pub game_id: i32,
    pub title: String,
    pub source_game_id: i32,
    #[serde(with = "crate::utils::datetime::millis")]
    pub exported_at_utc: DateTime<Utc>,
    pub tables: Vec<GameDataImportTable>,
    pub users: RosterOutcome,
    pub teams: RosterOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameDataImportTable {
    pub name: String,
    pub rows: u64,
    pub restored: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterOutcome {
    pub matched: u64,
    pub created: u64,
}
