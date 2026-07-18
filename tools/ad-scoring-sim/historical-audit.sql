-- Reproduce the database evidence quoted by the A&D scoring simulator.
--
-- Run with psql against the rsctf PostgreSQL database, for example:
--   psql "$RSCTF_DATABASE_URL" -X -v ON_ERROR_STOP=1 \
--     -f tools/ad-scoring-sim/historical-audit.sql
--
-- This script is aggregate-only. The transaction is READ ONLY and uses one
-- repeatable-read snapshot because active games add rounds while the audit runs.
-- It never selects flags, tokens, team names, service endpoints, or raw IDs.
--
-- Scope definitions used throughout:
--   * surviving game: a game_id that still has a row in "Games";
--   * deleted-game cohort: a game_id retained by "AdRounds" or
--     "AdTeamServices" but absent from "Games";
--   * historical attack/check: a row joined through a valid "AdRounds" row to
--     a deleted-game cohort.
--
-- Rows that cannot be joined to a round are intentionally excluded from cohort
-- evidence because their game cannot be established. The final integrity query
-- reports those exclusions separately. A missing game records deletion or
-- teardown, but SQL alone cannot prove that its traffic came from a particular
-- harness; the burst and topology queries provide the supporting signature.

\set ON_ERROR_STOP on
\pset pager off

BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY;
SET LOCAL statement_timeout = '60s';

-- Pin the exact snapshot used for every result below. Persist this timestamp in
-- results.json whenever the evidence constants are refreshed.
SELECT
    'snapshot' AS section,
    transaction_timestamp() AS snapshot_utc;

-- Base row counts make later exclusions auditable. These counts are expected to
-- grow while surviving games are active, but are stable inside this transaction.
SELECT
    'base_counts' AS section,
    (SELECT count(*) FROM "Games") AS games,
    (SELECT count(*) FROM "Participations") AS participations,
    (SELECT count(*) FROM "AdTeamServices") AS services,
    (SELECT count(*) FROM "AdRounds") AS rounds,
    (SELECT count(*) FROM "AdFlags") AS flags,
    (SELECT count(*) FROM "AdAttacks") AS attacks,
    (SELECT count(*) FROM "AdCheckResults") AS checks;

-- Evidence for "the two surviving games contain zero attacks". Attack rows are
-- scoped through their capture round, matching the scoreboard's game boundary.
SELECT
    'surviving_games' AS section,
    count(*) AS surviving_games,
    count(*) FILTER (WHERE attacks > 0) AS attacked_surviving_games,
    sum(attacks) AS surviving_game_attacks
FROM (
    SELECT
        game.id,
        (
            SELECT count(*)
            FROM "AdAttacks" attack
            JOIN "AdRounds" round_ ON round_.id = attack.round_id
            WHERE round_.game_id = game.id
        ) AS attacks
    FROM "Games" game
) scoped_games;

-- Deleted-game cohort counts and ranges. A cohort is counted even if teardown
-- left only services or only rounds. team_id_slots are pseudonymous
-- participation IDs retained in service rows, not surviving Participation rows.
WITH historical_ids AS (
    SELECT service.game_id
    FROM "AdTeamServices" service
    LEFT JOIN "Games" game ON game.id = service.game_id
    WHERE game.id IS NULL
    UNION
    SELECT round_.game_id
    FROM "AdRounds" round_
    LEFT JOIN "Games" game ON game.id = round_.game_id
    WHERE game.id IS NULL
), cohorts AS (
    SELECT
        historical_ids.game_id,
        (
            SELECT count(DISTINCT service.participation_id)
            FROM "AdTeamServices" service
            WHERE service.game_id = historical_ids.game_id
        ) AS teams,
        (
            SELECT count(*)
            FROM "AdTeamServices" service
            WHERE service.game_id = historical_ids.game_id
        ) AS services,
        (
            SELECT count(*)
            FROM "AdRounds" round_
            WHERE round_.game_id = historical_ids.game_id
        ) AS rounds,
        (
            SELECT count(*)
            FROM "AdFlags" flag
            JOIN "AdRounds" round_ ON round_.id = flag.round_id
            WHERE round_.game_id = historical_ids.game_id
        ) AS flags,
        (
            SELECT count(*)
            FROM "AdAttacks" attack
            JOIN "AdRounds" round_ ON round_.id = attack.round_id
            WHERE round_.game_id = historical_ids.game_id
        ) AS attacks,
        (
            SELECT count(*)
            FROM "AdCheckResults" check_
            JOIN "AdRounds" round_ ON round_.id = check_.round_id
            WHERE round_.game_id = historical_ids.game_id
        ) AS checks
    FROM historical_ids
)
SELECT
    'deleted_game_cohorts' AS section,
    count(*) AS cohorts,
    count(*) FILTER (WHERE attacks > 0) AS attacked_cohorts,
    sum(teams) AS team_id_slots,
    sum(services) AS services,
    sum(rounds) AS rounds,
    sum(flags) AS flags,
    sum(attacks) AS attacks,
    sum(checks) AS checks,
    min(teams) AS min_teams,
    percentile_cont(0.5) WITHIN GROUP (ORDER BY teams) AS median_teams,
    max(teams) AS max_teams,
    min(rounds) AS min_rounds,
    percentile_cont(0.5) WITHIN GROUP (ORDER BY rounds) AS median_rounds,
    max(rounds) AS max_rounds
FROM cohorts;

-- Field-size provenance. historicalFieldSizes in results.json is the ordered
-- set where attacked_cohorts > 0, not every observed deleted-game topology.
WITH historical_ids AS (
    SELECT service.game_id
    FROM "AdTeamServices" service
    LEFT JOIN "Games" game ON game.id = service.game_id
    WHERE game.id IS NULL
    UNION
    SELECT round_.game_id
    FROM "AdRounds" round_
    LEFT JOIN "Games" game ON game.id = round_.game_id
    WHERE game.id IS NULL
), cohorts AS (
    SELECT
        historical_ids.game_id,
        count(DISTINCT service.participation_id) AS teams,
        EXISTS (
            SELECT 1
            FROM "AdAttacks" attack
            JOIN "AdRounds" round_ ON round_.id = attack.round_id
            WHERE round_.game_id = historical_ids.game_id
        ) AS attacked
    FROM historical_ids
    LEFT JOIN "AdTeamServices" service
        ON service.game_id = historical_ids.game_id
    GROUP BY historical_ids.game_id
), field_sizes AS (
    SELECT
        teams,
        count(*) AS cohorts,
        count(*) FILTER (WHERE attacked) AS attacked_cohorts
    FROM cohorts
    GROUP BY teams
)
SELECT
    'deleted_game_field_sizes' AS section,
    teams,
    cohorts,
    attacked_cohorts
FROM field_sizes
ORDER BY teams;

WITH historical_ids AS (
    SELECT service.game_id
    FROM "AdTeamServices" service
    LEFT JOIN "Games" game ON game.id = service.game_id
    WHERE game.id IS NULL
    UNION
    SELECT round_.game_id
    FROM "AdRounds" round_
    LEFT JOIN "Games" game ON game.id = round_.game_id
    WHERE game.id IS NULL
), attacked_sizes AS (
    SELECT count(DISTINCT service.participation_id) AS teams
    FROM historical_ids
    JOIN "AdTeamServices" service
        ON service.game_id = historical_ids.game_id
    WHERE EXISTS (
        SELECT 1
        FROM "AdAttacks" attack
        JOIN "AdRounds" round_ ON round_.id = attack.round_id
        WHERE round_.game_id = historical_ids.game_id
    )
    GROUP BY historical_ids.game_id
)
SELECT
    'historical_field_sizes_constant' AS section,
    array_agg(DISTINCT teams ORDER BY teams) AS attacked_cohort_field_sizes
FROM attacked_sizes;

-- Capture counts and capturer multiplicity. Group by flag before counting so
-- this remains correct even if an older database predates attack de-duplication.
WITH historical_attacks AS (
    SELECT attack.flag_id, attack.attacker_participation_id
    FROM "AdAttacks" attack
    JOIN "AdRounds" round_ ON round_.id = attack.round_id
    LEFT JOIN "Games" game ON game.id = round_.game_id
    WHERE game.id IS NULL
), captured_flags AS (
    SELECT
        flag_id,
        count(*) AS capture_rows,
        count(DISTINCT attacker_participation_id) AS capturers
    FROM historical_attacks
    GROUP BY flag_id
)
SELECT
    'historical_capture_summary' AS section,
    (SELECT count(*) FROM historical_attacks) AS attack_rows,
    count(*) AS captured_flags,
    sum(capture_rows) AS capture_rows,
    count(*) FILTER (WHERE capturers = 1) AS one_capturer_flags,
    count(*) FILTER (WHERE capturers >= 2) AS two_or_more_capturer_flags,
    min(capturers) AS min_capturers,
    max(capturers) AS max_capturers
FROM captured_flags;

WITH historical_attacks AS (
    SELECT attack.flag_id, attack.attacker_participation_id
    FROM "AdAttacks" attack
    JOIN "AdRounds" round_ ON round_.id = attack.round_id
    LEFT JOIN "Games" game ON game.id = round_.game_id
    WHERE game.id IS NULL
), captured_flags AS (
    SELECT
        flag_id,
        count(DISTINCT attacker_participation_id) AS capturers
    FROM historical_attacks
    GROUP BY flag_id
)
SELECT
    'historical_capturer_histogram' AS section,
    capturers,
    count(*) AS flags
FROM captured_flags
GROUP BY capturers
ORDER BY capturers;

-- Include uncaptured flags to show how little of the planted-flag population the
-- deleted-game traffic exercised. This is scoped only to valid deleted-game rounds.
WITH historical_flags AS (
    SELECT flag.id
    FROM "AdFlags" flag
    JOIN "AdRounds" round_ ON round_.id = flag.round_id
    LEFT JOIN "Games" game ON game.id = round_.game_id
    WHERE game.id IS NULL
), historical_attacks AS (
    SELECT attack.flag_id, attack.attacker_participation_id
    FROM "AdAttacks" attack
    JOIN "AdRounds" capture_round ON capture_round.id = attack.round_id
    LEFT JOIN "Games" game ON game.id = capture_round.game_id
    WHERE game.id IS NULL
), multiplicity AS (
    SELECT
        historical_flags.id,
        count(DISTINCT attack.attacker_participation_id) AS capturers
    FROM historical_flags
    LEFT JOIN historical_attacks attack ON attack.flag_id = historical_flags.id
    GROUP BY historical_flags.id
)
SELECT
    'historical_all_flag_multiplicity' AS section,
    capturers,
    count(*) AS flags,
    round(100.0 * count(*) / sum(count(*)) OVER (), 4) AS percent
FROM multiplicity
GROUP BY capturers
ORDER BY capturers;

-- Checker denominator behind historicalInternalErrorFraction. Only checks with
-- a valid round in a deleted-game cohort are included. Status values follow
-- AdCheckStatus: 0 Ok, 1 Mumble, 2 Offline, 3 InternalError.
WITH historical_checks AS (
    SELECT check_.status, check_.sla_credit
    FROM "AdCheckResults" check_
    JOIN "AdRounds" round_ ON round_.id = check_.round_id
    LEFT JOIN "Games" game ON game.id = round_.game_id
    WHERE game.id IS NULL
)
SELECT
    'deleted_game_checker_summary' AS section,
    count(*) AS checker_rows,
    count(*) FILTER (WHERE status = 3) AS internal_error_rows,
    count(*) FILTER (WHERE status = 3 AND sla_credit IS NULL)
        AS internal_error_null_credit_placeholders,
    round(
        count(*) FILTER (WHERE status = 3)::numeric / NULLIF(count(*), 0),
        5
    ) AS internal_error_fraction,
    count(*) FILTER (WHERE sla_credit IS NULL) AS null_credit_rows,
    count(*) FILTER (WHERE sla_credit = 0) AS zero_credit_rows,
    count(*) FILTER (WHERE sla_credit > 0) AS positive_credit_rows
FROM historical_checks;

WITH historical_checks AS (
    SELECT check_.status, check_.sla_credit
    FROM "AdCheckResults" check_
    JOIN "AdRounds" round_ ON round_.id = check_.round_id
    LEFT JOIN "Games" game ON game.id = round_.game_id
    WHERE game.id IS NULL
)
SELECT
    'deleted_game_checker_statuses' AS section,
    status,
    CASE status
        WHEN 0 THEN 'Ok'
        WHEN 1 THEN 'Mumble'
        WHEN 2 THEN 'Offline'
        WHEN 3 THEN 'InternalError'
        ELSE 'Unknown'
    END AS status_name,
    count(*) AS rows,
    count(*) FILTER (WHERE sla_credit IS NULL) AS null_credit_rows,
    count(*) FILTER (WHERE sla_credit = 0) AS zero_credit_rows,
    count(*) FILTER (WHERE sla_credit > 0) AS positive_credit_rows,
    round(100.0 * count(*) / sum(count(*)) OVER (), 3) AS percent
FROM historical_checks
GROUP BY status
ORDER BY status;

-- There is no positive SLA evidence in the cohorts that also contain attacks.
-- This is a separate denominator from all deleted-game checker rows above.
WITH attacked_cohorts AS (
    SELECT DISTINCT round_.game_id
    FROM "AdAttacks" attack
    JOIN "AdRounds" round_ ON round_.id = attack.round_id
    LEFT JOIN "Games" game ON game.id = round_.game_id
    WHERE game.id IS NULL
), attacked_cohort_checks AS (
    SELECT check_.status, check_.sla_credit
    FROM "AdCheckResults" check_
    JOIN "AdRounds" round_ ON round_.id = check_.round_id
    JOIN attacked_cohorts ON attacked_cohorts.game_id = round_.game_id
)
SELECT
    'attacked_cohort_sla_evidence' AS section,
    count(*) AS checker_rows,
    count(*) FILTER (WHERE sla_credit IS NULL) AS null_credit_rows,
    count(*) FILTER (WHERE sla_credit = 0) AS zero_credit_rows,
    count(*) FILTER (WHERE sla_credit > 0) AS positive_credit_rows,
    coalesce(sum(sla_credit), 0) AS total_sla_credit
FROM attacked_cohort_checks;

-- Timing signatures supporting the load-traffic classification. A cohort span
-- measures from its first accepted capture to its last; capture latency measures
-- from flag planting to submission. No team or game identifiers are emitted.
WITH historical_attacks AS (
    SELECT round_.game_id, attack.submitted_at
    FROM "AdAttacks" attack
    JOIN "AdRounds" round_ ON round_.id = attack.round_id
    LEFT JOIN "Games" game ON game.id = round_.game_id
    WHERE game.id IS NULL
), cohort_spans AS (
    SELECT
        game_id,
        count(*) AS captures,
        extract(epoch FROM (max(submitted_at) - min(submitted_at))) AS span_seconds
    FROM historical_attacks
    GROUP BY game_id
)
SELECT
    'historical_attack_bursts' AS section,
    count(*) AS attacked_cohorts,
    count(*) FILTER (WHERE span_seconds <= 1) AS cohorts_at_most_one_second,
    count(*) FILTER (WHERE span_seconds > 1) AS cohorts_over_one_second,
    round(min(span_seconds)::numeric, 3) AS min_span_seconds,
    round(
        percentile_cont(0.5) WITHIN GROUP (ORDER BY span_seconds)::numeric,
        3
    ) AS median_span_seconds,
    round(max(span_seconds)::numeric, 3) AS max_span_seconds
FROM cohort_spans;

WITH capture_latency AS (
    SELECT extract(epoch FROM (attack.submitted_at - flag.planted_at)) AS seconds
    FROM "AdAttacks" attack
    JOIN "AdRounds" capture_round ON capture_round.id = attack.round_id
    LEFT JOIN "Games" game ON game.id = capture_round.game_id
    JOIN "AdFlags" flag ON flag.id = attack.flag_id
    WHERE game.id IS NULL
)
SELECT
    'historical_capture_latency' AS section,
    count(*) AS captures,
    round(min(seconds)::numeric, 3) AS min_seconds,
    round(percentile_cont(0.5) WITHIN GROUP (ORDER BY seconds)::numeric, 3)
        AS p50_seconds,
    round(percentile_cont(0.9) WITHIN GROUP (ORDER BY seconds)::numeric, 3)
        AS p90_seconds,
    round(percentile_cont(0.99) WITHIN GROUP (ORDER BY seconds)::numeric, 3)
        AS p99_seconds,
    round(max(seconds)::numeric, 3) AS max_seconds
FROM capture_latency;

-- A capture can legitimately use a still-live flag planted in an earlier round.
-- Report that lag rather than treating a round mismatch as corruption.
WITH capture_lag AS (
    SELECT capture_round.number - planted_round.number AS round_lag
    FROM "AdAttacks" attack
    JOIN "AdRounds" capture_round ON capture_round.id = attack.round_id
    LEFT JOIN "Games" game ON game.id = capture_round.game_id
    JOIN "AdFlags" flag ON flag.id = attack.flag_id
    JOIN "AdRounds" planted_round ON planted_round.id = flag.round_id
    WHERE game.id IS NULL
)
SELECT
    'historical_capture_round_lag' AS section,
    round_lag,
    count(*) AS captures
FROM capture_lag
GROUP BY round_lag
ORDER BY round_lag;

-- Participation availability and attacker concentration. No historical attack
-- row has a surviving attacker Participation; same-game service rows retain
-- enough pseudonymous identity to reconstruct every observed attacker/victim edge.
WITH historical_attacks AS (
    SELECT attack.*, round_.game_id
    FROM "AdAttacks" attack
    JOIN "AdRounds" round_ ON round_.id = attack.round_id
    LEFT JOIN "Games" game ON game.id = round_.game_id
    WHERE game.id IS NULL
)
SELECT
    'historical_participation_availability' AS section,
    count(*) AS attack_rows,
    count(*) FILTER (
        WHERE EXISTS (
            SELECT 1
            FROM "Participations" participation
            WHERE participation.id = historical_attacks.attacker_participation_id
        )
    ) AS attacks_with_participation_row,
    count(*) FILTER (
        WHERE EXISTS (
            SELECT 1
            FROM "AdTeamServices" service
            WHERE service.game_id = historical_attacks.game_id
              AND service.participation_id = historical_attacks.attacker_participation_id
        )
    ) AS attacks_with_same_game_attacker_service,
    count(*) FILTER (
        WHERE EXISTS (
            SELECT 1
            FROM "AdTeamServices" service
            WHERE service.game_id = historical_attacks.game_id
              AND service.id = historical_attacks.victim_team_service_id
        )
    ) AS attacks_with_same_game_victim_service,
    count(*) FILTER (
        WHERE EXISTS (
            SELECT 1
            FROM "AdTeamServices" attacker_service
            WHERE attacker_service.game_id = historical_attacks.game_id
              AND attacker_service.participation_id =
                  historical_attacks.attacker_participation_id
        )
          AND EXISTS (
            SELECT 1
            FROM "AdTeamServices" victim_service
            WHERE victim_service.game_id = historical_attacks.game_id
              AND victim_service.id = historical_attacks.victim_team_service_id
        )
    ) AS fully_service_reconstructable_attacks
FROM historical_attacks;

WITH historical_attacks AS (
    SELECT attack.attacker_participation_id, attack.flag_id
    FROM "AdAttacks" attack
    JOIN "AdRounds" round_ ON round_.id = attack.round_id
    LEFT JOIN "Games" game ON game.id = round_.game_id
    WHERE game.id IS NULL
), attackers AS (
    SELECT
        attacker_participation_id,
        count(*) AS captures,
        count(DISTINCT flag_id) AS captured_flags
    FROM historical_attacks
    GROUP BY attacker_participation_id
)
SELECT
    'historical_attacker_distribution' AS section,
    count(*) AS attacker_ids,
    min(captures) AS min_captures,
    percentile_cont(0.5) WITHIN GROUP (ORDER BY captures) AS median_captures,
    percentile_cont(0.9) WITHIN GROUP (ORDER BY captures) AS p90_captures,
    percentile_cont(0.99) WITHIN GROUP (ORDER BY captures) AS p99_captures,
    max(captures) AS max_captures,
    sum(captures) AS total_captures,
    count(*) FILTER (WHERE captures = 1) AS one_capture_attackers
FROM attackers;

-- Raw integrity inventory. These rows are not silently folded into historical
-- cohort denominators. Missing Games are the deleted-game cohorts analyzed
-- above; missing rounds/services are unusable orphans for scoring reconstruction.
SELECT
    'orphan_exclusions' AS section,
    (
        SELECT count(*)
        FROM "AdRounds" round_
        LEFT JOIN "Games" game ON game.id = round_.game_id
        WHERE game.id IS NULL
    ) AS rounds_missing_game,
    (
        SELECT count(*)
        FROM "AdTeamServices" service
        LEFT JOIN "Games" game ON game.id = service.game_id
        WHERE game.id IS NULL
    ) AS services_missing_game,
    (
        SELECT count(*)
        FROM "AdFlags" flag
        LEFT JOIN "AdRounds" round_ ON round_.id = flag.round_id
        WHERE round_.id IS NULL
    ) AS flags_missing_round,
    (
        SELECT count(*)
        FROM "AdFlags" flag
        LEFT JOIN "AdTeamServices" service ON service.id = flag.team_service_id
        WHERE service.id IS NULL
    ) AS flags_missing_service,
    (
        SELECT count(*)
        FROM "AdCheckResults" check_
        LEFT JOIN "AdRounds" round_ ON round_.id = check_.round_id
        WHERE round_.id IS NULL
    ) AS checks_missing_round,
    (
        SELECT count(*)
        FROM "AdCheckResults" check_
        LEFT JOIN "AdTeamServices" service ON service.id = check_.team_service_id
        WHERE service.id IS NULL
    ) AS checks_missing_service,
    (
        SELECT count(*)
        FROM "AdAttacks" attack
        LEFT JOIN "AdRounds" round_ ON round_.id = attack.round_id
        WHERE round_.id IS NULL
    ) AS attacks_missing_round,
    (
        SELECT count(*)
        FROM "AdAttacks" attack
        LEFT JOIN "AdFlags" flag ON flag.id = attack.flag_id
        WHERE flag.id IS NULL
    ) AS attacks_missing_flag,
    (
        SELECT count(*)
        FROM "AdAttacks" attack
        LEFT JOIN "AdTeamServices" service
            ON service.id = attack.victim_team_service_id
        WHERE service.id IS NULL
    ) AS attacks_missing_victim_service;

ROLLBACK;
