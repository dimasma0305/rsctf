//! Restore of a competition data archive into a new hidden game.
//!
//! The definition import and every restored row share one transaction, so a
//! rejected row leaves nothing behind. Identifiers are rewritten through the
//! maps recorded while the roster and each id-bearing table are inserted, in
//! the dependency order of [`spec::catalog`].

use std::collections::{BTreeMap, HashMap};

use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseTransaction, Statement};
use serde_json::{json, Map as JsonMap, Value as JsonValue};

use super::spec::{catalog, JsonRewrite, Map, Restore, RowsRestore, TableSpec};
use super::*;
use crate::controllers::edit::{
    parse_definition_entries, persist_game_import_with, read_archive_upload,
    read_game_import_archive_with_limits, GameImportLimits, ImportedDefinition, GAME_IMPORT_SLOTS,
};

pub(crate) const MAX_DATA_IMPORT_ROWS_PER_TABLE: usize = 500_000;
const DATA_IMPORT_LIMITS: GameImportLimits = GameImportLimits {
    entries: 8_192,
    file_bytes: 256 * 1024 * 1024,
    total_bytes: 256 * 1024 * 1024,
    compression_ratio: 200,
    path_components: 32,
};
const INSERT_BATCH_ROWS: usize = 500;

pub(super) type Row = JsonMap<String, JsonValue>;

/// Every `data/*.jsonl` entry, parsed and bounded, keyed by archive name.
pub(super) struct ArchiveTables {
    rows: BTreeMap<&'static str, Vec<Row>>,
}

impl ArchiveTables {
    pub(super) fn rows(&self, name: &str) -> &[Row] {
        self.rows.get(name).map(Vec::as_slice).unwrap_or_default()
    }
}

/// Identifier maps and per-table outcomes accumulated during one restore.
pub(super) struct RestoreContext {
    pub(super) game_id: i32,
    maps: HashMap<Map, HashMap<String, JsonValue>>,
    pub(super) restored: BTreeMap<&'static str, u64>,
}

impl RestoreContext {
    pub(super) fn new(game_id: i32) -> Self {
        Self {
            game_id,
            maps: HashMap::new(),
            restored: BTreeMap::new(),
        }
    }

    pub(super) fn record(&mut self, map: Map, old: &JsonValue, new: JsonValue) {
        self.maps
            .entry(map)
            .or_default()
            .insert(old.to_string(), new);
    }

    /// Rewrite one identifier. `null` stays `null`; an unknown identifier is
    /// an archive integrity error, never a silent dangling reference.
    pub(super) fn mapped(
        &self,
        map: Map,
        old: &JsonValue,
        table: &str,
        column: &str,
    ) -> AppResult<JsonValue> {
        if old.is_null() {
            return Ok(JsonValue::Null);
        }
        self.maps
            .get(&map)
            .and_then(|entries| entries.get(&old.to_string()))
            .cloned()
            .ok_or_else(|| {
                AppError::bad_request(format!(
                    "data/{table}.jsonl references unknown {} {old} in {column}",
                    map.name()
                ))
            })
    }

    pub(super) fn count(&mut self, name: &'static str, rows: u64) {
        *self.restored.entry(name).or_default() += rows;
    }
}

pub(super) fn statement(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DatabaseBackend::Postgres, sql, values)
}

pub(super) fn restore_error(table: &str, error: impl std::fmt::Display) -> AppError {
    AppError::bad_request(format!("data/{table}.jsonl could not be restored: {error}"))
}

/// `POST /api/edit/games/import/data` — restore a competition data archive
/// as a new hidden game. Admin only.
pub async fn import_game_data(
    State(st): State<SharedState>,
    AdminUser(admin): AdminUser,
    multipart: Multipart,
) -> AppResult<RequestResponse<GameDataImportResult>> {
    let bytes = read_archive_upload(multipart).await?;
    let _permit = GAME_IMPORT_SLOTS
        .try_acquire()
        .map_err(|_| AppError::unavailable("Game import capacity is busy; retry shortly"))?;
    let result = restore_archive(&st, bytes, admin.id).await?;
    Ok(RequestResponse::ok(result))
}

/// Expand, validate, and restore one uploaded archive. The caller holds an
/// import slot.
pub(super) async fn restore_archive(
    st: &SharedState,
    bytes: Vec<u8>,
    actor: Uuid,
) -> AppResult<GameDataImportResult> {
    let entries = tokio::task::spawn_blocking(move || {
        read_game_import_archive_with_limits(&bytes, DATA_IMPORT_LIMITS)
    })
    .await
    .map_err(|error| AppError::internal(format!("data import task failed: {error}")))??;
    let manifest = parse_manifest(&entries)?;
    let (export_game, export_challenges) = parse_definition_entries(&entries)?;
    if export_game.end_time_utc > Utc::now() {
        return Err(AppError::bad_request(
            "Competition data can only be restored for an event whose end time has passed",
        ));
    }
    let tables = parse_tables(&entries)?;
    let staged_writeups = roster::stage_writeups(st, &entries, tables.rows("writeups")).await?;

    let (definition, context) = persist_game_import_with(
        st,
        &entries,
        &export_game,
        &export_challenges,
        move |transaction, definition| {
            Box::pin(restore_all(
                transaction,
                definition,
                tables,
                staged_writeups,
                actor,
            ))
        },
    )
    .await?;

    let (users, teams, context) = context;
    let result_tables = catalog()
        .map(|spec| GameDataImportTable {
            name: spec.name.to_string(),
            rows: context.restored.get(spec.name).copied().unwrap_or(0),
            restored: !matches!(spec.restore, Restore::ExportOnly),
        })
        .collect();
    Ok(GameDataImportResult {
        game_id: definition.game_id,
        title: export_game.title,
        source_game_id: manifest.source_game_id,
        exported_at_utc: manifest.exported_at_utc,
        tables: result_tables,
        users,
        teams,
    })
}

pub(super) fn parse_manifest(
    entries: &BTreeMap<String, Vec<u8>>,
) -> AppResult<DataArchiveManifest> {
    let bytes = entries.get("manifest.json").ok_or_else(|| {
        AppError::bad_request("Missing manifest.json: not a competition data archive")
    })?;
    let manifest: DataArchiveManifest = serde_json::from_slice(bytes)
        .map_err(|error| AppError::bad_request(format!("Invalid manifest.json: {error}")))?;
    if manifest.kind != ARCHIVE_KIND {
        return Err(AppError::bad_request(format!(
            "Archive kind {:?} is not a competition data archive",
            manifest.kind
        )));
    }
    if manifest.format_version != ARCHIVE_FORMAT_VERSION {
        return Err(AppError::bad_request(format!(
            "Unsupported competition data archive format version {}",
            manifest.format_version
        )));
    }
    Ok(manifest)
}

pub(super) fn parse_tables(entries: &BTreeMap<String, Vec<u8>>) -> AppResult<ArchiveTables> {
    let mut rows = BTreeMap::new();
    for (name, bytes) in entries {
        let Some(stem) = name.strip_prefix("data/") else {
            continue;
        };
        if name.ends_with('/') {
            continue;
        }
        let table_name = stem
            .strip_suffix(".jsonl")
            .ok_or_else(|| AppError::bad_request(format!("Unexpected archive entry {name}")))?;
        let spec = TableSpec::find(table_name)
            .ok_or_else(|| AppError::bad_request(format!("Unknown archive table {table_name}")))?;
        rows.insert(spec.name, parse_rows(spec.name, bytes)?);
    }
    Ok(ArchiveTables { rows })
}

pub(super) fn parse_rows(name: &str, bytes: &[u8]) -> AppResult<Vec<Row>> {
    let mut rows = Vec::new();
    for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if rows.len() >= MAX_DATA_IMPORT_ROWS_PER_TABLE {
            return Err(AppError::payload_too_large(format!(
                "data/{name}.jsonl exceeds the {MAX_DATA_IMPORT_ROWS_PER_TABLE} row import limit"
            )));
        }
        let value: JsonValue = serde_json::from_slice(line).map_err(|error| {
            AppError::bad_request(format!("data/{name}.jsonl line {}: {error}", index + 1))
        })?;
        match value {
            JsonValue::Object(row) => rows.push(row),
            _ => {
                return Err(AppError::bad_request(format!(
                    "data/{name}.jsonl line {} is not an object",
                    index + 1
                )))
            }
        }
    }
    Ok(rows)
}

type RestoreOutcome = (RosterOutcome, RosterOutcome, RestoreContext);

async fn restore_all(
    transaction: &DatabaseTransaction,
    definition: &ImportedDefinition,
    tables: ArchiveTables,
    staged_writeups: Vec<roster::StagedWriteup>,
    actor: Uuid,
) -> AppResult<RestoreOutcome> {
    // Restored memberships and placeholder accounts carry no login or join
    // evidence of their own. Opt this transaction into the identity-neutral
    // provisioning path that OAuth and bulk user import already use, so the
    // anti-cheat ledger guards accept the rows without inventing observations.
    transaction
        .execute(statement(
            "SELECT set_config('rsctf.identity_neutral_insert', '1', true)",
            Vec::new(),
        ))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let mut context = RestoreContext::new(definition.game_id);
    for (old, new) in &definition.challenge_ids {
        context.record(Map::Challenge, &json!(old), json!(new));
    }
    let (users, teams) = roster::restore_roster(
        transaction,
        definition,
        &tables,
        &staged_writeups,
        actor,
        &mut context,
    )
    .await?;
    for spec in catalog() {
        match spec.restore {
            Restore::Roster | Restore::ExportOnly => {}
            Restore::Rows(restore) => {
                restore_rows(
                    transaction,
                    spec,
                    restore,
                    tables.rows(spec.name),
                    &mut context,
                )
                .await?;
            }
        }
    }
    recount_challenges(transaction, definition.game_id).await?;
    Ok((users, teams, context))
}

/// Insert the rows of one generic table in bounded batches, allocating fresh
/// serial ids up front so dependent tables can be rewritten deterministically.
async fn restore_rows(
    transaction: &DatabaseTransaction,
    spec: &TableSpec,
    restore: RowsRestore,
    rows: &[Row],
    context: &mut RestoreContext,
) -> AppResult<()> {
    let columns = spec.columns();
    let mut insert_columns = Vec::with_capacity(columns.len() + 1);
    for column in &columns {
        match column.identifier {
            Some("id") if restore.id.is_none() => {}
            Some(identifier) => insert_columns.push(identifier),
            None => {
                return Err(AppError::internal(format!(
                    "archive table {} declares a joined column but restores rows",
                    spec.name
                )))
            }
        }
    }
    if restore.game_column {
        insert_columns.push("game_id");
    }
    let quoted = insert_columns
        .iter()
        .map(|column| format!("\"{column}\""))
        .collect::<Vec<_>>();
    let sql = format!(
        "INSERT INTO \"{table}\" ({columns}) SELECT {selected} FROM jsonb_populate_recordset(NULL::\"{table}\", $1) AS r",
        table = spec.table,
        columns = quoted.join(", "),
        selected = quoted
            .iter()
            .map(|column| format!("r.{column}"))
            .collect::<Vec<_>>()
            .join(", "),
    );

    for chunk in rows.chunks(INSERT_BATCH_ROWS) {
        let ids = match restore.id {
            Some(_) => allocate_ids(transaction, spec, chunk.len()).await?,
            None => Vec::new(),
        };
        let mut objects = Vec::with_capacity(chunk.len());
        for (index, row) in chunk.iter().enumerate() {
            let mut object = JsonMap::with_capacity(insert_columns.len());
            for column in &columns {
                let Some(identifier) = column.identifier else {
                    continue;
                };
                if identifier == "id" {
                    if let Some(map) = restore.id {
                        let allocated = json!(ids[index]);
                        let old = row.get("id").cloned().unwrap_or(JsonValue::Null);
                        if old.is_null() {
                            return Err(restore_error(spec.name, "a row has no id"));
                        }
                        context.record(map, &old, allocated.clone());
                        object.insert("id".to_string(), allocated);
                    }
                    continue;
                }
                let mut value = row.get(&column.key).cloned().unwrap_or(JsonValue::Null);
                if let Some((_, map)) = restore
                    .remaps
                    .iter()
                    .find(|(remapped, _)| *remapped == identifier)
                {
                    value = context.mapped(*map, &value, spec.name, &column.key)?;
                }
                object.insert(identifier.to_string(), value);
            }
            if restore.game_column {
                object.insert("game_id".to_string(), json!(context.game_id));
            }
            if let Some(rewrite) = restore.json_rewrite {
                rewrite_json(rewrite, spec.name, &mut object, context)?;
            }
            objects.push(JsonValue::Object(object));
        }
        transaction
            .execute(statement(
                &sql,
                vec![sea_orm::Value::Json(Some(Box::new(JsonValue::Array(
                    objects,
                ))))],
            ))
            .await
            .map_err(|error| restore_error(spec.name, error))?;
        context.count(spec.name, chunk.len() as u64);
    }
    Ok(())
}

async fn allocate_ids(
    transaction: &DatabaseTransaction,
    spec: &TableSpec,
    count: usize,
) -> AppResult<Vec<i64>> {
    let rows = transaction
        .query_all(statement(
            "SELECT nextval(pg_get_serial_sequence($1, 'id'))::bigint AS id FROM generate_series(1, $2::int)",
            vec![
                format!("\"{}\"", spec.table).into(),
                (i32::try_from(count).map_err(|_| restore_error(spec.name, "batch too large"))?)
                    .into(),
            ],
        ))
        .await
        .map_err(|error| restore_error(spec.name, error))?;
    let ids = rows
        .iter()
        .map(|row| row.try_get::<i64>("", "id"))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| restore_error(spec.name, error))?;
    if ids.len() != count {
        return Err(restore_error(spec.name, "identifier allocation was short"));
    }
    Ok(ids)
}

pub(super) fn rewrite_json(
    rewrite: JsonRewrite,
    table: &str,
    object: &mut Row,
    context: &RestoreContext,
) -> AppResult<()> {
    match rewrite {
        JsonRewrite::KothOfficialConfig => {
            let roster = object
                .get("roster_snapshot")
                .and_then(JsonValue::as_array)
                .cloned()
                .ok_or_else(|| restore_error(table, "rosterSnapshot must be an array"))?;
            let roster = roster
                .iter()
                .map(|id| context.mapped(Map::Participation, id, table, "rosterSnapshot"))
                .collect::<AppResult<Vec<_>>>()?;
            object.insert("roster_snapshot".to_string(), JsonValue::Array(roster));
            let hills = object
                .get("hills_snapshot")
                .and_then(JsonValue::as_array)
                .cloned()
                .ok_or_else(|| restore_error(table, "hillsSnapshot must be an array"))?;
            let mut rewritten = Vec::with_capacity(hills.len());
            for hill in hills {
                let JsonValue::Object(mut hill) = hill else {
                    return Err(restore_error(
                        table,
                        "hillsSnapshot entries must be objects",
                    ));
                };
                let old = hill.get("challengeId").cloned().unwrap_or(JsonValue::Null);
                hill.insert(
                    "challengeId".to_string(),
                    context.mapped(Map::Challenge, &old, table, "hillsSnapshot")?,
                );
                rewritten.push(JsonValue::Object(hill));
            }
            object.insert("hills_snapshot".to_string(), JsonValue::Array(rewritten));
        }
    }
    Ok(())
}

/// Solve and submission counters are derived from the restored rows rather
/// than trusted from the archive.
async fn recount_challenges(transaction: &DatabaseTransaction, game_id: i32) -> AppResult<()> {
    transaction
        .execute(statement(
            r#"UPDATE "GameChallenges" challenge
                  SET accepted_count = (
                        SELECT COUNT(*)::integer
                          FROM "FirstSolves" first_solve
                          JOIN "Submissions" submission
                            ON submission.id = first_solve.submission_id
                           AND submission.participation_id = first_solve.participation_id
                           AND submission.challenge_id = first_solve.challenge_id
                           AND submission.game_id = challenge.game_id
                           AND submission.status = $2
                         WHERE first_solve.challenge_id = challenge.id
                      ),
                      submission_count = (
                        SELECT COUNT(*)::integer
                          FROM "Submissions" submission
                         WHERE submission.challenge_id = challenge.id
                           AND submission.game_id = challenge.game_id
                      )
                WHERE challenge.game_id = $1"#,
            vec![
                game_id.into(),
                (crate::utils::enums::AnswerResult::Accepted as i16).into(),
            ],
        ))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}
