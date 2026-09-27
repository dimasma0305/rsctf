//! Declarative catalog of the game-scoped tables carried by a competition data
//! archive. The same specification selects rows for export and drives the
//! remapped restore, so the two directions cannot drift apart.
//!
//! Every column name here is a static identifier from the schema; user data
//! only ever travels through bound parameters.

/// Foreign-key namespace whose identifiers change between installations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Map {
    Challenge,
    Division,
    Participation,
    Team,
    User,
    Submission,
    Finding,
    AdRound,
    AdTeamService,
    AdFlag,
    KothTarget,
    KothCycle,
    KothToken,
}

impl Map {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Challenge => "challenge",
            Self::Division => "division",
            Self::Participation => "participation",
            Self::Team => "team",
            Self::User => "user",
            Self::Submission => "submission",
            Self::Finding => "antiCheatFinding",
            Self::AdRound => "adRound",
            Self::AdTeamService => "adTeamService",
            Self::AdFlag => "adFlag",
            Self::KothTarget => "kothTarget",
            Self::KothCycle => "kothCycle",
            Self::KothToken => "kothToken",
        }
    }
}

/// How the rows of one table return to a database.
#[derive(Clone, Copy)]
pub(super) enum Restore {
    /// Roster tables are restored by identity-aware code in `roster.rs`.
    Roster,
    /// Generic remapped insert.
    Rows(RowsRestore),
    /// Exported for reference only; the rows depend on deployment-local secrets
    /// or executables and are never written back.
    ExportOnly,
}

#[derive(Clone, Copy)]
pub(super) struct RowsRestore {
    /// When set, fresh serial ids are allocated and recorded in this map so
    /// dependent tables can follow.
    pub(super) id: Option<Map>,
    /// Whether the table carries `game_id`, rewritten to the imported game.
    pub(super) game_column: bool,
    /// Foreign-key columns rewritten through the named map.
    pub(super) remaps: &'static [(&'static str, Map)],
    /// JSON columns whose embedded identifiers need a custom rewrite.
    pub(super) json_rewrite: Option<JsonRewrite>,
}

#[derive(Clone, Copy)]
pub(super) enum JsonRewrite {
    /// `KothOfficialConfigs`: `roster_snapshot` is an array of participation
    /// ids and `hills_snapshot` an array of `{challengeId, serviceWeight}`.
    KothOfficialConfig,
}

/// One archived table.
#[derive(Clone, Copy)]
pub(super) struct TableSpec {
    /// Archive name, also the `data/<name>.jsonl` entry.
    pub(super) name: &'static str,
    /// PostgreSQL table.
    pub(super) table: &'static str,
    /// Optional `FROM` override (the table is aliased `t`).
    pub(super) from: Option<&'static str>,
    /// `WHERE` fragment selecting the rows of game `$1`.
    pub(super) scope: &'static str,
    /// Deterministic `ORDER BY` fragment for offset pagination.
    pub(super) order: &'static str,
    /// Selected columns: a bare identifier, or `expr AS key` for joins.
    pub(super) columns: &'static [&'static str],
    pub(super) restore: Restore,
}

const GAME: &str = "t.game_id = $1";
const VIA_AD_ROUND: &str =
    r#"EXISTS (SELECT 1 FROM "AdRounds" r WHERE r.id = t.round_id AND r.game_id = $1)"#;
const VIA_KOTH_CYCLE: &str =
    r#"EXISTS (SELECT 1 FROM "KothCrownCycles" c WHERE c.id = t.cycle_id AND c.game_id = $1)"#;
const VIA_TEAM: &str =
    r#"t.team_id IN (SELECT p.team_id FROM "Participations" p WHERE p.game_id = $1)"#;
const USERS_SCOPE: &str = r#"t.id IN (
    SELECT m.user_id FROM "TeamMembers" m
      JOIN "Participations" p ON p.team_id = m.team_id WHERE p.game_id = $1
    UNION SELECT up.user_id FROM "UserParticipations" up WHERE up.game_id = $1
    UNION SELECT tm.captain_id FROM "Teams" tm
      JOIN "Participations" p ON p.team_id = tm.id WHERE p.game_id = $1
    UNION SELECT s.user_id FROM "Submissions" s
     WHERE s.game_id = $1 AND s.user_id IS NOT NULL
    UNION SELECT r.user_id FROM "ChallengeReviews" r WHERE r.game_id = $1
    UNION SELECT w.graded_by FROM "WriteupGrades" w
     WHERE w.game_id = $1 AND w.graded_by IS NOT NULL
    UNION SELECT f.reviewed_by_user_id FROM "AntiCheatFindingReviews" f
     WHERE f.game_id = $1
    UNION SELECT a.submitted_by FROM "AiChatLinks" a
     WHERE a.game_id = $1 AND a.submitted_by IS NOT NULL
    UNION SELECT e.user_id FROM "AiChatLinkEvents" e
     WHERE e.game_id = $1 AND e.user_id IS NOT NULL
  )"#;

const fn rows(
    id: Option<Map>,
    game_column: bool,
    remaps: &'static [(&'static str, Map)],
) -> Restore {
    Restore::Rows(RowsRestore {
        id,
        game_column,
        remaps,
        json_rewrite: None,
    })
}

const PARTICIPATION: (&str, Map) = ("participation_id", Map::Participation);
const CHALLENGE: (&str, Map) = ("challenge_id", Map::Challenge);
const USER: (&str, Map) = ("user_id", Map::User);
const AD_ROUND: (&str, Map) = ("ad_round_id", Map::AdRound);
const ROUND: (&str, Map) = ("round_id", Map::AdRound);
const TEAM_SERVICE: (&str, Map) = ("team_service_id", Map::AdTeamService);
const CYCLE: (&str, Map) = ("cycle_id", Map::KothCycle);
const TOKEN: (&str, Map) = ("token_id", Map::KothToken);

mod attack_defense;
mod export_only;
mod jeopardy;
mod koth;
mod roster;
mod telemetry;

/// Archive order doubles as restore order: every table appears after the
/// tables whose identifiers it references.
const GROUPS: &[&[TableSpec]] = &[
    roster::ROSTER,
    jeopardy::JEOPARDY,
    attack_defense::ATTACK_DEFENSE,
    koth::KOTH,
    telemetry::TELEMETRY,
    export_only::EXPORT_ONLY,
];

pub(super) fn catalog() -> impl Iterator<Item = &'static TableSpec> {
    GROUPS.iter().flat_map(|group| group.iter())
}

/// One selected column: the SQL expression and its archive key.
pub(super) struct Column {
    pub(super) sql: String,
    pub(super) key: String,
    /// The bare column identifier when the entry is a plain column.
    pub(super) identifier: Option<&'static str>,
}

pub(super) fn archive_key(identifier: &str) -> String {
    let mut key = String::with_capacity(identifier.len());
    let mut upper_next = false;
    for (index, ch) in identifier.chars().enumerate() {
        if ch == '_' {
            upper_next = true;
        } else if upper_next {
            key.extend(ch.to_uppercase());
            upper_next = false;
        } else if index == 0 {
            key.extend(ch.to_lowercase());
        } else {
            key.push(ch);
        }
    }
    key
}

impl TableSpec {
    pub(super) fn entry(&self) -> String {
        format!("data/{}.jsonl", self.name)
    }

    pub(super) fn columns(&self) -> Vec<Column> {
        self.columns
            .iter()
            .map(|entry| match entry.split_once(" AS ") {
                Some((expression, key)) => Column {
                    sql: expression.to_string(),
                    key: key.to_string(),
                    identifier: None,
                },
                None => Column {
                    sql: format!("t.\"{entry}\""),
                    key: archive_key(entry),
                    identifier: Some(entry),
                },
            })
            .collect()
    }

    /// Paged row selection, each row as one JSON text with archive keys.
    pub(super) fn select_sql(&self) -> String {
        let projection = self
            .columns()
            .into_iter()
            .map(|column| format!("{} AS \"{}\"", column.sql, column.key))
            .collect::<Vec<_>>()
            .join(", ");
        let from = self
            .from
            .map(str::to_string)
            .unwrap_or_else(|| format!("\"{}\" t", self.table));
        format!(
            "SELECT row_to_json(x)::text FROM (SELECT {projection} FROM {from} WHERE {scope} ORDER BY {order} LIMIT $2 OFFSET $3) x",
            scope = self.scope,
            order = self.order,
        )
    }

    pub(super) fn find(name: &str) -> Option<&'static TableSpec> {
        catalog().find(|spec| spec.name == name)
    }
}
