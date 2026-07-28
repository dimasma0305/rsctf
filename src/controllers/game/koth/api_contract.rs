//! Fixed wire contract and normalization for API-native KotH arenas.

use std::collections::HashSet;

use serde::Deserialize;

use crate::utils::error::{AppError, AppResult};

pub(super) const MAX_BODY_BYTES: usize = 512 * 1_024;
pub(super) const MAX_TEAM_ENTRIES: usize =
    crate::services::ad::engine::koth_api::MAX_API_ARENA_TEAMS;
pub(super) const MAX_OBJECTIVES: usize = 16;
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
    pub(super) integrity: EvidenceRatioInput,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct KothArenaSnapshotInput {
    pub(super) context: String,
    pub(super) teams: Vec<TeamEvidenceInput>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NormalizedInputRow {
    pub(super) token_hash: [u8; 32],
    pub(super) activity_earned: i64,
    pub(super) activity_possible: i64,
    pub(super) objective_earned: i64,
    pub(super) objective_possible: i64,
    pub(super) valid_actions: i64,
    pub(super) total_actions: i64,
    pub(super) objective_count: i16,
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
            "KotH arena snapshot may contain at most {MAX_TEAM_ENTRIES} teams"
        )));
    }

    let mut token_hashes = HashSet::with_capacity(input.teams.len());
    let expected_objective_count = input.teams.first().map(|team| team.objectives.len());
    for team in &input.teams {
        let token_hash = parse_token_hash(&team.token_hash)?;
        if !token_hashes.insert(token_hash) {
            return Err(AppError::bad_request(
                "KotH arena token hashes must be unique",
            ));
        }
        validate_ratio(&team.activity, "activity")?;
        validate_ratio(&team.integrity, "integrity")?;
        if team.objectives.is_empty() || team.objectives.len() > MAX_OBJECTIVES {
            return Err(AppError::bad_request(format!(
                "each KotH arena team must have between 1 and {MAX_OBJECTIVES} objective components"
            )));
        }
        if Some(team.objectives.len()) != expected_objective_count {
            return Err(AppError::bad_request(
                "every team in a KotH arena snapshot must use the same objective component count",
            ));
        }
        for objective in &team.objectives {
            validate_ratio(objective, "objective")?;
        }
    }
    Ok(input)
}

pub(super) fn flatten(team: TeamEvidenceInput) -> NormalizedInputRow {
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
        valid_actions: team.integrity.earned,
        total_actions: team.integrity.possible,
        objective_count: team.objectives.len() as i16,
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
            integrity: EvidenceRatioInput {
                earned: 19,
                possible: 20,
            },
        };
        let row = flatten(input);
        assert_eq!(
            (row.objective_earned, row.objective_possible),
            (1_000_000, 2_000_000)
        );
        assert_eq!((row.activity_earned, row.activity_possible), (4, 5));
        assert_eq!((row.valid_actions, row.total_actions), (19, 20));
    }

    #[test]
    fn objective_normalization_rounds_deterministically_without_floats() {
        let row = flatten(TeamEvidenceInput {
            token_hash: "b".repeat(64),
            activity: EvidenceRatioInput {
                earned: 1,
                possible: 1,
            },
            objectives: vec![EvidenceRatioInput {
                earned: 1,
                possible: 3,
            }],
            integrity: EvidenceRatioInput {
                earned: 1,
                possible: 1,
            },
        });
        assert_eq!(
            (row.objective_earned, row.objective_possible),
            (333_333, 1_000_000)
        );
    }

    #[test]
    fn empty_snapshot_explicitly_represents_no_active_teams() {
        let body = serde_json::to_vec(&serde_json::json!({
            "context": context(),
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
                "objectives":[{"earned":1,"possible":1}],
                "integrity":{"earned":1,"possible":1}
            }]),
            serde_json::json!([{
                "tokenHash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "activity":{"earned":1,"possible":1},
                "objectives":[],
                "integrity":{"earned":1,"possible":1}
            }]),
            serde_json::json!([
                {
                    "tokenHash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "activity":{"earned":1,"possible":1},
                    "objectives":[{"earned":1,"possible":1}],
                    "integrity":{"earned":1,"possible":1}
                },
                {
                    "tokenHash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "activity":{"earned":1,"possible":1},
                    "objectives":[{"earned":1,"possible":1}],
                    "integrity":{"earned":1,"possible":1}
                }
            ]),
            serde_json::json!([
                {
                    "tokenHash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "activity":{"earned":1,"possible":1},
                    "objectives":[{"earned":1,"possible":1}],
                    "integrity":{"earned":1,"possible":1}
                },
                {
                    "tokenHash":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "activity":{"earned":1,"possible":1},
                    "objectives":[
                        {"earned":1,"possible":1},
                        {"earned":1,"possible":1}
                    ],
                    "integrity":{"earned":1,"possible":1}
                }
            ]),
        ] {
            let body = serde_json::to_vec(&serde_json::json!({
                "context": context(),
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
            "teams": [],
            "points": 100,
        }))
        .unwrap();
        assert!(parse_and_normalize(&body).is_err());
    }
}
