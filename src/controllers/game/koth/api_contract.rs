//! Fixed wire contract and normalization for Leaderboard KotH evidence.

use std::collections::HashSet;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::utils::error::{AppError, AppResult};

pub(super) const MAX_BODY_BYTES: usize = 512 * 1_024;
pub(super) const MAX_TEAM_ENTRIES: usize =
    crate::services::ad::engine::koth_api::MAX_LEADERBOARD_TEAMS;
pub(super) const MAX_OBJECTIVES: usize = 16;
const MAX_OBJECTIVE_ID_BYTES: usize = 64;
const MAX_EVIDENCE_BUDGET: i64 = 1_000_000_000_000;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct EvidenceRatioInput {
    pub(super) earned: i64,
    pub(super) possible: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct TeamEvidenceInput {
    pub(super) token_hash: String,
    pub(super) activity: EvidenceRatioInput,
    pub(super) objectives: Vec<EvidenceRatioInput>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct KothArenaSnapshotInput {
    pub(super) context: String,
    pub(super) objective_ids: Vec<String>,
    pub(super) teams: Vec<TeamEvidenceInput>,
}

impl KothArenaSnapshotInput {
    pub(super) fn objective_schema_hash(&self) -> [u8; 32] {
        objective_schema_hash(&self.objective_ids)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NormalizedInputRow {
    pub(super) token_hash: [u8; 32],
    pub(super) activity_earned: i64,
    pub(super) activity_possible: i64,
    pub(super) objective_earned: i64,
    pub(super) objective_possible: i64,
    pub(super) objective_count: i16,
}

pub(super) fn objective_schema_hash(objective_ids: &[String]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update((objective_ids.len() as u64).to_be_bytes());
    for objective_id in objective_ids {
        digest.update((objective_id.len() as u64).to_be_bytes());
        digest.update(objective_id.as_bytes());
    }
    digest.finalize().into()
}

fn canonical_objective_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_OBJECTIVE_ID_BYTES
        && bytes[0].is_ascii_lowercase()
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(*byte, b'-' | b'_' | b'.')
        })
}

fn canonical_context(context: &str) -> bool {
    context.len() == 64
        && context
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn parse_token_hash(value: &str) -> AppResult<[u8; 32]> {
    if !canonical_context(value) {
        return Err(AppError::bad_request(
            "tokenHash must be 64 lowercase hexadecimal characters",
        ));
    }
    let decoded = hex::decode(value)
        .map_err(|_| AppError::bad_request("tokenHash must be valid hexadecimal"))?;
    decoded
        .try_into()
        .map_err(|_| AppError::bad_request("tokenHash must encode 32 bytes"))
}

fn validate_ratio(ratio: &EvidenceRatioInput, field: &'static str) -> AppResult<()> {
    if ratio.earned < 0
        || ratio.possible <= 0
        || ratio.earned > ratio.possible
        || ratio.possible > MAX_EVIDENCE_BUDGET
    {
        return Err(AppError::bad_request(format!(
            "{field} evidence must satisfy 0 <= earned <= possible <= {MAX_EVIDENCE_BUDGET}"
        )));
    }
    Ok(())
}

pub(super) fn parse_and_normalize(body: &[u8]) -> AppResult<KothArenaSnapshotInput> {
    let input: KothArenaSnapshotInput =
        serde_json::from_slice(body).map_err(|_| AppError::bad_request("invalid JSON body"))?;
    if !canonical_context(&input.context) {
        return Err(AppError::bad_request("invalid KotH observer context"));
    }
    if input.teams.len() > MAX_TEAM_ENTRIES {
        return Err(AppError::bad_request(format!(
            "Leaderboard snapshot may contain at most {MAX_TEAM_ENTRIES} teams"
        )));
    }
    if input.objective_ids.is_empty() || input.objective_ids.len() > MAX_OBJECTIVES {
        return Err(AppError::bad_request(format!(
            "Leaderboard objectiveIds must contain between 1 and {MAX_OBJECTIVES} stable IDs"
        )));
    }
    let mut objective_ids = HashSet::with_capacity(input.objective_ids.len());
    for objective_id in &input.objective_ids {
        if !canonical_objective_id(objective_id) || !objective_ids.insert(objective_id) {
            return Err(AppError::bad_request(
                "Leaderboard objectiveIds must be unique lowercase IDs of at most 64 bytes",
            ));
        }
    }

    let mut token_hashes = HashSet::with_capacity(input.teams.len());
    for team in &input.teams {
        let token_hash = parse_token_hash(&team.token_hash)?;
        if !token_hashes.insert(token_hash) {
            return Err(AppError::bad_request(
                "Leaderboard token hashes must be unique",
            ));
        }
        validate_ratio(&team.activity, "activity")?;
        if team.objectives.len() != input.objective_ids.len() {
            return Err(AppError::bad_request(
                "every Leaderboard team row must match objectiveIds exactly and in order",
            ));
        }
        for objective in &team.objectives {
            validate_ratio(objective, "objective")?;
        }
    }
    Ok(input)
}

pub(super) fn flatten(team: TeamEvidenceInput, objective_count: usize) -> NormalizedInputRow {
    // Normalize each native objective independently before averaging. Summing
    // raw budgets would let a 10,000-point metric drown out a 10-point metric
    // even when both are intended to be equally important.
    let objective_earned = team.objectives.iter().fold(0_i64, |sum, objective| {
        let scale =
            crate::services::ad::engine::koth_api::API_OBJECTIVE_NORMALIZATION_SCALE as i128;
        let scaled = ((objective.earned as i128 * scale) + objective.possible as i128 / 2)
            / objective.possible as i128;
        sum + scaled as i64
    });
    let objective_possible =
        crate::services::ad::engine::koth_api::API_OBJECTIVE_NORMALIZATION_SCALE
            * team.objectives.len() as i64;
    NormalizedInputRow {
        token_hash: parse_token_hash(&team.token_hash)
            .expect("snapshot validation already accepted tokenHash"),
        activity_earned: team.activity.earned,
        activity_possible: team.activity.possible,
        objective_earned,
        objective_possible,
        objective_count: objective_count as i16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> String {
        "a".repeat(64)
    }

    #[test]
    fn platform_normalizes_each_native_objective_before_averaging() {
        let input = TeamEvidenceInput {
            token_hash: "a".repeat(64),
            activity: EvidenceRatioInput {
                earned: 4,
                possible: 5,
            },
            objectives: vec![
                EvidenceRatioInput {
                    earned: 100,
                    possible: 1_000,
                },
                EvidenceRatioInput {
                    earned: 9,
                    possible: 10,
                },
            ],
        };
        let row = flatten(input, 2);
        assert_eq!(
            (row.objective_earned, row.objective_possible),
            (1_000_000, 2_000_000)
        );
        assert_eq!((row.activity_earned, row.activity_possible), (4, 5));
    }

    #[test]
    fn objective_normalization_rounds_deterministically_without_floats() {
        let row = flatten(
            TeamEvidenceInput {
                token_hash: "b".repeat(64),
                activity: EvidenceRatioInput {
                    earned: 1,
                    possible: 1,
                },
                objectives: vec![EvidenceRatioInput {
                    earned: 1,
                    possible: 3,
                }],
            },
            1,
        );
        assert_eq!(
            (row.objective_earned, row.objective_possible),
            (333_333, 1_000_000)
        );
    }

    #[test]
    fn empty_snapshot_explicitly_represents_no_active_teams() {
        let body = serde_json::to_vec(&serde_json::json!({
            "context": context(),
            "objectiveIds": ["throughput"],
            "teams": [],
        }))
        .unwrap();
        assert!(parse_and_normalize(&body).unwrap().teams.is_empty());
    }

    #[test]
    fn malformed_or_inflating_evidence_is_rejected() {
        for teams in [
            serde_json::json!([{
                "tokenHash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "activity":{"earned":2,"possible":1},
                "objectives":[{"earned":1,"possible":1}]
            }]),
            serde_json::json!([{
                "tokenHash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "activity":{"earned":1,"possible":1},
                "objectives":[]
            }]),
            serde_json::json!([
                {
                    "tokenHash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "activity":{"earned":1,"possible":1},
                    "objectives":[{"earned":1,"possible":1}]
                },
                {
                    "tokenHash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "activity":{"earned":1,"possible":1},
                    "objectives":[{"earned":1,"possible":1}]
                }
            ]),
            serde_json::json!([
                {
                    "tokenHash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "activity":{"earned":1,"possible":1},
                    "objectives":[{"earned":1,"possible":1}]
                },
                {
                    "tokenHash":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "activity":{"earned":1,"possible":1},
                    "objectives":[
                        {"earned":1,"possible":1},
                        {"earned":1,"possible":1}
                    ]
                }
            ]),
        ] {
            let body = serde_json::to_vec(&serde_json::json!({
                "context": context(),
                "objectiveIds": ["throughput"],
                "teams": teams,
            }))
            .unwrap();
            assert!(parse_and_normalize(&body).is_err());
        }
    }

    #[test]
    fn unknown_fields_and_noncanonical_contexts_are_rejected() {
        let body = serde_json::to_vec(&serde_json::json!({
            "context": "A".repeat(64),
            "objectiveIds": ["throughput"],
            "teams": [],
            "points": 100,
        }))
        .unwrap();
        assert!(parse_and_normalize(&body).is_err());
    }

    #[test]
    fn objective_identity_and_order_are_cryptographically_distinct() {
        let first = vec!["quality".to_string(), "latency".to_string()];
        let reordered = vec!["latency".to_string(), "quality".to_string()];
        assert_ne!(
            objective_schema_hash(&first),
            objective_schema_hash(&reordered)
        );

        for invalid in [
            serde_json::json!([]),
            serde_json::json!(["Quality"]),
            serde_json::json!(["quality", "quality"]),
        ] {
            let body = serde_json::to_vec(&serde_json::json!({
                "context": context(),
                "objectiveIds": invalid,
                "teams": [],
            }))
            .unwrap();
            assert!(parse_and_normalize(&body).is_err());
        }
    }
}
