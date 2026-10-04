//! Identity-aware restore of users, teams, memberships, participations, and
//! writeups. Accounts keep their archived UUID so evidence stays attributable;
//! serial team and participation ids are always reassigned.

use sea_orm::{ConnectionTrait, DatabaseTransaction};
use serde_json::{json, Value as JsonValue};

use super::import::{restore_error, statement, ArchiveTables, RestoreContext, Row};
use super::spec::Map;
use super::*;
use crate::controllers::edit::ImportedDefinition;
use crate::services::blob_refs::StagedBlob;

const USERS: &str = "users";
const TEAMS: &str = "teams";
const TEAM_MEMBERS: &str = "teamMembers";
const DIVISIONS: &str = "divisions";
const PARTICIPATIONS: &str = "participations";
const USER_PARTICIPATIONS: &str = "userParticipations";
const WRITEUPS: &str = "writeups";
const CHALLENGE_STATES: &str = "challengeStates";
const GAME_STATE: &str = "gameState";

/// A writeup blob staged from `files/{hash}` before the transaction opens.
pub(super) struct StagedWriteup {
    pub(super) participation_id: JsonValue,
    pub(super) blob: StagedBlob,
}

fn text(row: &Row, key: &str) -> Option<String> {
    row.get(key)
        .and_then(JsonValue::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty())
}

fn integer(row: &Row, key: &str, table: &str) -> AppResult<i64> {
    row.get(key)
        .and_then(JsonValue::as_i64)
        .ok_or_else(|| restore_error(table, format!("{key} must be an integer")))
}

fn boolean(row: &Row, key: &str) -> bool {
    row.get(key).and_then(JsonValue::as_bool).unwrap_or(false)
}

fn uuid_value(row: &Row, key: &str, table: &str) -> AppResult<Uuid> {
    text(row, key)
        .and_then(|value| Uuid::parse_str(&value).ok())
        .ok_or_else(|| restore_error(table, format!("{key} must be a UUID")))
}

fn mapped_uuid(
    context: &RestoreContext,
    old: &JsonValue,
    table: &str,
    column: &str,
) -> AppResult<Uuid> {
    let mapped = context.mapped(Map::User, old, table, column)?;
    mapped
        .as_str()
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| restore_error(table, format!("{column} did not map to a UUID")))
}

fn mapped_i32(
    context: &RestoreContext,
    map: Map,
    old: &JsonValue,
    table: &str,
    column: &str,
) -> AppResult<i32> {
    let mapped = context.mapped(map, old, table, column)?;
    mapped
        .as_i64()
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| restore_error(table, format!("{column} did not map to an integer")))
}

/// Stage every bundled writeup blob referenced by the archive. Missing or
/// hash-mismatched bundles are skipped: a participation then simply has no
/// writeup, exactly like an unbundled attachment.
pub(super) async fn stage_writeups(
    st: &SharedState,
    entries: &std::collections::BTreeMap<String, Vec<u8>>,
    rows: &[Row],
) -> AppResult<Vec<StagedWriteup>> {
    let mut staged = Vec::new();
    for (ordinal, row) in rows.iter().enumerate() {
        let Some(hash) = text(row, "hash") else {
            continue;
        };
        let Some(bytes) = entries
            .get(&format!("files/{hash}"))
            .filter(|bytes| crate::utils::codec::sha256_hex(bytes) == hash)
        else {
            continue;
        };
        let name = text(row, "fileName").unwrap_or_else(|| hash.clone());
        let blob = crate::services::blob_refs::stage_blob(
            st.pg(),
            st.storage.as_ref(),
            Uuid::new_v4(),
            &format!("game-data-import:writeup:{ordinal}"),
            None,
            &name,
            bytes,
        )
        .await?;
        staged.push(StagedWriteup {
            participation_id: row
                .get("participationId")
                .cloned()
                .unwrap_or(JsonValue::Null),
            blob,
        });
    }
    Ok(staged)
}

pub(super) async fn restore_roster(
    transaction: &DatabaseTransaction,
    definition: &ImportedDefinition,
    tables: &ArchiveTables,
    staged_writeups: &[StagedWriteup],
    actor: Uuid,
    context: &mut RestoreContext,
) -> AppResult<(RosterOutcome, RosterOutcome)> {
    restore_divisions(definition, tables.rows(DIVISIONS), context)?;
    let users = restore_users(transaction, tables.rows(USERS), context).await?;
    let teams = restore_teams(transaction, tables.rows(TEAMS), actor, context).await?;
    restore_team_members(transaction, tables.rows(TEAM_MEMBERS), context).await?;
    restore_participations(
        transaction,
        definition,
        tables.rows(PARTICIPATIONS),
        staged_writeups,
        context,
    )
    .await?;
    restore_user_participations(transaction, tables.rows(USER_PARTICIPATIONS), context).await?;
    restore_challenge_states(transaction, tables.rows(CHALLENGE_STATES), context).await?;
    restore_game_state(transaction, tables.rows(GAME_STATE), context).await?;
    Ok((users, teams))
}

/// Official engine state locked at the competition boundary. The definition
/// import clears it (a template must never start scoring); the data archive
/// carries it so the restored boards settle exactly like the source.
async fn restore_game_state(
    transaction: &DatabaseTransaction,
    rows: &[Row],
    context: &mut RestoreContext,
) -> AppResult<()> {
    let Some(row) = rows.first() else {
        return Ok(());
    };
    let optional_round = |key: &str| -> AppResult<Option<i32>> {
        match row.get(key) {
            Some(JsonValue::Null) | None => Ok(None),
            Some(value) => value
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())
                .map(Some)
                .ok_or_else(|| restore_error(GAME_STATE, format!("{key} must be an integer"))),
        }
    };
    transaction
        .execute(statement(
            r#"UPDATE "Games"
                  SET ad_scoring_start_round = $1,
                      koth_scoring_start_round = $2,
                      ad_scoring_paused = $3
                WHERE id = $4"#,
            vec![
                optional_round("adScoringStartRound")?.into(),
                optional_round("kothScoringStartRound")?.into(),
                boolean(row, "adScoringPaused").into(),
                context.game_id.into(),
            ],
        ))
        .await
        .map_err(|error| restore_error(GAME_STATE, error))?;
    context.count(GAME_STATE, 1);
    Ok(())
}

/// Divisions are created by the definition import in archive order; the
/// `divisions` table carries the source ids in that same order.
fn restore_divisions(
    definition: &ImportedDefinition,
    rows: &[Row],
    context: &mut RestoreContext,
) -> AppResult<()> {
    if rows.len() != definition.division_ids.len() {
        return Err(restore_error(
            DIVISIONS,
            "division count differs from game.json",
        ));
    }
    for (row, new_id) in rows.iter().zip(&definition.division_ids) {
        let old = row.get("id").cloned().unwrap_or(JsonValue::Null);
        if old.is_null() {
            return Err(restore_error(DIVISIONS, "a row has no id"));
        }
        context.record(Map::Division, &old, json!(new_id));
    }
    context.count(DIVISIONS, rows.len() as u64);
    Ok(())
}

async fn find_user(
    transaction: &DatabaseTransaction,
    sql: &str,
    value: sea_orm::Value,
) -> AppResult<Option<Uuid>> {
    let row = transaction
        .query_one(statement(sql, vec![value]))
        .await
        .map_err(|error| restore_error(USERS, error))?;
    row.map(|row| row.try_get::<Uuid>("", "id"))
        .transpose()
        .map_err(|error| restore_error(USERS, error))
}

/// Match by archived id, then e-mail, then username; otherwise create a
/// placeholder account with the archived id and no password.
async fn restore_users(
    transaction: &DatabaseTransaction,
    rows: &[Row],
    context: &mut RestoreContext,
) -> AppResult<RosterOutcome> {
    let mut outcome = RosterOutcome::default();
    for row in rows {
        let archived = uuid_value(row, "id", USERS)?;
        let user_name =
            text(row, "userName").ok_or_else(|| restore_error(USERS, "userName is required"))?;
        let email = text(row, "email");
        let mut matched = find_user(
            transaction,
            r#"SELECT id FROM "AspNetUsers" WHERE id = $1"#,
            archived.into(),
        )
        .await?;
        if matched.is_none() {
            if let Some(email) = &email {
                matched = find_user(
                    transaction,
                    r#"SELECT id FROM "AspNetUsers" WHERE normalized_email = upper($1) ORDER BY id LIMIT 1"#,
                    email.clone().into(),
                )
                .await?;
            }
        }
        if matched.is_none() {
            matched = find_user(
                transaction,
                r#"SELECT id FROM "AspNetUsers" WHERE normalized_user_name = upper($1)"#,
                user_name.clone().into(),
            )
            .await?;
        }
        let resolved = match matched {
            Some(id) => {
                outcome.matched += 1;
                id
            }
            None => {
                transaction
                    .execute(statement(
                        r#"INSERT INTO "AspNetUsers"
                             (id, user_name, normalized_user_name, email, normalized_email,
                              email_confirmed, password_hash, security_stamp, concurrency_stamp,
                              phone_number, phone_number_confirmed, two_factor_enabled, lockout_end,
                              lockout_enabled, access_failed_count, role, ip, browser_fingerprint,
                              last_signed_in_utc, last_visited_utc, register_time_utc, bio,
                              real_name, std_number, exercise_visible, avatar_hash)
                           VALUES
                             ($1, $2, upper($2), $3, upper($3), FALSE, NULL, $4, $4,
                              NULL, FALSE, FALSE, NULL, FALSE, 0, $5, '', NULL,
                              $6, $6, $6, $7, $8, $9, TRUE, NULL)"#,
                        vec![
                            archived.into(),
                            user_name.into(),
                            email.into(),
                            crate::utils::codec::random_hex(16).into(),
                            (Role::User as i16).into(),
                            Utc::now().into(),
                            text(row, "bio").unwrap_or_default().into(),
                            text(row, "realName").unwrap_or_default().into(),
                            text(row, "stdNumber").unwrap_or_default().into(),
                        ],
                    ))
                    .await
                    .map_err(|error| restore_error(USERS, error))?;
                outcome.created += 1;
                archived
            }
        };
        context.record(
            Map::User,
            &json!(archived.to_string()),
            json!(resolved.to_string()),
        );
    }
    context.count(USERS, rows.len() as u64);
    Ok(outcome)
}

/// Reuse a team only when the same id still carries the same name on this
/// installation; otherwise create a new team captained by the archived
/// captain, falling back to the importing administrator.
async fn restore_teams(
    transaction: &DatabaseTransaction,
    rows: &[Row],
    actor: Uuid,
    context: &mut RestoreContext,
) -> AppResult<RosterOutcome> {
    let mut outcome = RosterOutcome::default();
    for row in rows {
        let old = row.get("id").cloned().unwrap_or(JsonValue::Null);
        let archived_id = i32::try_from(integer(row, "id", TEAMS)?)
            .map_err(|error| restore_error(TEAMS, error))?;
        let name = text(row, "name").ok_or_else(|| restore_error(TEAMS, "name is required"))?;
        let existing = transaction
            .query_one(statement(
                r#"SELECT id FROM "Teams" WHERE id = $1 AND name = $2"#,
                vec![archived_id.into(), name.clone().into()],
            ))
            .await
            .map_err(|error| restore_error(TEAMS, error))?;
        let resolved = match existing {
            Some(existing) => {
                outcome.matched += 1;
                existing
                    .try_get::<i32>("", "id")
                    .map_err(|error| restore_error(TEAMS, error))?
            }
            None => {
                let captain = match row.get("captainId") {
                    Some(captain) if !captain.is_null() => {
                        mapped_uuid(context, captain, TEAMS, "captainId").unwrap_or(actor)
                    }
                    _ => actor,
                };
                let created = transaction
                    .query_one(statement(
                        r#"INSERT INTO "Teams"
                             (name, bio, avatar_hash, locked, invite_token, captain_id)
                           VALUES ($1, $2, NULL, $3, $4, $5)
                        RETURNING id"#,
                        vec![
                            name.into(),
                            text(row, "bio").into(),
                            boolean(row, "locked").into(),
                            crate::utils::codec::random_hex(16).into(),
                            captain.into(),
                        ],
                    ))
                    .await
                    .map_err(|error| restore_error(TEAMS, error))?
                    .ok_or_else(|| restore_error(TEAMS, "insert returned no id"))?;
                outcome.created += 1;
                created
                    .try_get::<i32>("", "id")
                    .map_err(|error| restore_error(TEAMS, error))?
            }
        };
        context.record(Map::Team, &old, json!(resolved));
    }
    context.count(TEAMS, rows.len() as u64);
    Ok(outcome)
}

async fn restore_team_members(
    transaction: &DatabaseTransaction,
    rows: &[Row],
    context: &mut RestoreContext,
) -> AppResult<()> {
    for row in rows {
        let team = mapped_i32(
            context,
            Map::Team,
            row.get("teamId").unwrap_or(&JsonValue::Null),
            TEAM_MEMBERS,
            "teamId",
        )?;
        let user = mapped_uuid(
            context,
            row.get("userId").unwrap_or(&JsonValue::Null),
            TEAM_MEMBERS,
            "userId",
        )?;
        transaction
            .execute(statement(
                r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES ($1, $2)
                   ON CONFLICT (team_id, user_id) DO NOTHING"#,
                vec![team.into(), user.into()],
            ))
            .await
            .map_err(|error| restore_error(TEAM_MEMBERS, error))?;
    }
    context.count(TEAM_MEMBERS, rows.len() as u64);
    Ok(())
}

/// Participations are always new rows of the imported game; their scoreboard
/// token is re-signed with the imported game's private key.
async fn restore_participations(
    transaction: &DatabaseTransaction,
    definition: &ImportedDefinition,
    rows: &[Row],
    staged_writeups: &[StagedWriteup],
    context: &mut RestoreContext,
) -> AppResult<()> {
    for row in rows {
        let old = row.get("id").cloned().unwrap_or(JsonValue::Null);
        if old.is_null() {
            return Err(restore_error(PARTICIPATIONS, "a row has no id"));
        }
        let team = mapped_i32(
            context,
            Map::Team,
            row.get("teamId").unwrap_or(&JsonValue::Null),
            PARTICIPATIONS,
            "teamId",
        )?;
        let division = match row.get("divisionId") {
            Some(division) if !division.is_null() => Some(mapped_i32(
                context,
                Map::Division,
                division,
                PARTICIPATIONS,
                "divisionId",
            )?),
            _ => None,
        };
        let status = i16::try_from(integer(row, "status", PARTICIPATIONS)?)
            .map_err(|error| restore_error(PARTICIPATIONS, error))?;
        let suspicion = i32::try_from(
            row.get("suspicionScore")
                .and_then(JsonValue::as_i64)
                .unwrap_or(0),
        )
        .map_err(|error| restore_error(PARTICIPATIONS, error))?;
        let writeup_id = match staged_writeups
            .iter()
            .find(|staged| staged.participation_id == old)
        {
            Some(staged) => Some(
                crate::services::blob_refs::publish_staged_blob_in_seaorm_transaction(
                    transaction,
                    &staged.blob,
                )
                .await?,
            ),
            None => None,
        };
        let token =
            crate::controllers::game::participation_token_from_key(&definition.private_key, team)?;
        let created = transaction
            .query_one(statement(
                // `competitive_admitted_at_utc` and `reconciliation_version` are
                // database-owned (trigger-assigned) and are never restored.
                r#"INSERT INTO "Participations"
                     (status, token, writeup_id, game_id, team_id, division_id,
                      suspicion_score)
                   VALUES ($1, $2, $3, $4, $5, $6, $7)
                RETURNING id"#,
                vec![
                    status.into(),
                    token.into(),
                    writeup_id.into(),
                    definition.game_id.into(),
                    team.into(),
                    division.into(),
                    suspicion.into(),
                ],
            ))
            .await
            .map_err(|error| restore_error(PARTICIPATIONS, error))?
            .ok_or_else(|| restore_error(PARTICIPATIONS, "insert returned no id"))?;
        let new_id = created
            .try_get::<i32>("", "id")
            .map_err(|error| restore_error(PARTICIPATIONS, error))?;
        context.record(Map::Participation, &old, json!(new_id));
    }
    context.count(PARTICIPATIONS, rows.len() as u64);
    context.count(WRITEUPS, staged_writeups.len() as u64);
    Ok(())
}

async fn restore_user_participations(
    transaction: &DatabaseTransaction,
    rows: &[Row],
    context: &mut RestoreContext,
) -> AppResult<()> {
    for row in rows {
        let user = mapped_uuid(
            context,
            row.get("userId").unwrap_or(&JsonValue::Null),
            USER_PARTICIPATIONS,
            "userId",
        )?;
        let team = mapped_i32(
            context,
            Map::Team,
            row.get("teamId").unwrap_or(&JsonValue::Null),
            USER_PARTICIPATIONS,
            "teamId",
        )?;
        let participation = mapped_i32(
            context,
            Map::Participation,
            row.get("participationId").unwrap_or(&JsonValue::Null),
            USER_PARTICIPATIONS,
            "participationId",
        )?;
        transaction
            .execute(statement(
                r#"INSERT INTO "UserParticipations" (user_id, game_id, team_id, participation_id)
                   VALUES ($1, $2, $3, $4)
                   ON CONFLICT (user_id, game_id) DO NOTHING"#,
                vec![
                    user.into(),
                    context.game_id.into(),
                    team.into(),
                    participation.into(),
                ],
            ))
            .await
            .map_err(|error| restore_error(USER_PARTICIPATIONS, error))?;
    }
    context.count(USER_PARTICIPATIONS, rows.len() as u64);
    Ok(())
}

/// The definition import disables every challenge; the archived enabled
/// state is what the boards were computed against. Counters are recomputed
/// from the restored rows afterwards.
async fn restore_challenge_states(
    transaction: &DatabaseTransaction,
    rows: &[Row],
    context: &mut RestoreContext,
) -> AppResult<()> {
    for row in rows {
        let challenge = mapped_i32(
            context,
            Map::Challenge,
            row.get("challengeId").unwrap_or(&JsonValue::Null),
            CHALLENGE_STATES,
            "challengeId",
        )?;
        transaction
            .execute(statement(
                r#"UPDATE "GameChallenges" SET is_enabled = $1 WHERE id = $2 AND game_id = $3"#,
                vec![
                    boolean(row, "isEnabled").into(),
                    challenge.into(),
                    context.game_id.into(),
                ],
            ))
            .await
            .map_err(|error| restore_error(CHALLENGE_STATES, error))?;
    }
    context.count(CHALLENGE_STATES, rows.len() as u64);
    Ok(())
}
