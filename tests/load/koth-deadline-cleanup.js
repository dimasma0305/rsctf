const SNAPSHOT_FIELDS = Object.freeze([
  "hillCount",
  "latestCycleCount",
  "completedCleanupReceipts",
  "endedReceipts",
  "deadlineSnapshotReceipts",
  "invalidDeadlineSnapshots",
  "invalidTerminalReceipts",
  "unfinalizedTerminalCycles",
  "liveTokens",
  "claimStates",
  "dirtyTargets",
  "liveContainerRows",
  "sharedContainerReferences",
  "unreleasedCooldowns",
]);

function gameIdentifier(value) {
  const gameId = Number(value);
  if (!Number.isSafeInteger(gameId) || gameId < 1) {
    throw new TypeError("KotH deadline cleanup requires a positive game id");
  }
  return gameId;
}

function count(value, label) {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new TypeError(`${label} must be a non-negative safe integer`);
  }
  return value;
}

// One read-only snapshot covers every durable/runtime identity that deadline
// recovery must clear. Historical cycles, tokens, observations, acquisitions,
// and receipts remain in place; only live state is counted as a blocker.
export function kothDeadlineCleanupQuery(value) {
  const gameId = gameIdentifier(value);
  return (
    `WITH hills AS (` +
      `SELECT target.id AS target_id,target.challenge_id ` +
        `FROM "KothTargets" target WHERE target.game_id=${gameId}` +
    `), latest_cycles AS (` +
      `SELECT DISTINCT ON (cycle.challenge_id) cycle.id,cycle.challenge_id,` +
        `cycle.phase,cycle.reset_attempt ` +
        `FROM "KothCrownCycles" cycle ` +
       `WHERE cycle.game_id=${gameId} ` +
       `ORDER BY cycle.challenge_id,cycle.cycle_number DESC` +
    `), cycle_containers AS (` +
      `SELECT cycle.old_container_id AS container_id ` +
        `FROM "KothCrownCycles" cycle WHERE cycle.game_id=${gameId} ` +
      `UNION SELECT cycle.replacement_container_id ` +
        `FROM "KothCrownCycles" cycle WHERE cycle.game_id=${gameId}` +
    `) SELECT jsonb_build_object(` +
      `'hillCount',(SELECT count(*) FROM hills),` +
      `'latestCycleCount',(SELECT count(*) FROM latest_cycles),` +
      `'completedCleanupReceipts',(` +
        `SELECT count(*) FROM latest_cycles cycle ` +
        `WHERE cycle.phase='Completed' AND EXISTS (` +
          `SELECT 1 FROM "KothCycleAuditReceipts" receipt ` +
           `WHERE receipt.cycle_id=cycle.id AND receipt.phase='DeadlineCleanup' ` +
             `AND receipt.attempt=cycle.reset_attempt` +
        `)` +
      `),` +
      `'endedReceipts',(` +
        `SELECT count(*) FROM latest_cycles cycle ` +
        `WHERE cycle.phase='Ended' AND EXISTS (` +
          `SELECT 1 FROM "KothCycleAuditReceipts" receipt ` +
           `WHERE receipt.cycle_id=cycle.id AND receipt.phase='Ended' ` +
             `AND receipt.attempt=cycle.reset_attempt` +
        `)` +
      `),` +
      `'deadlineSnapshotReceipts',(` +
        `SELECT count(*) FROM latest_cycles cycle WHERE EXISTS (` +
          `SELECT 1 FROM "KothCycleAuditReceipts" receipt ` +
           `WHERE receipt.cycle_id=cycle.id AND receipt.phase='DeadlineSnapshot' ` +
             `AND receipt.attempt=cycle.reset_attempt AND (` +
               `(receipt.receipt->>'status'='captured' ` +
                 `AND jsonb_typeof(receipt.filesystem_diff)='array') OR (` +
               `receipt.receipt->>'status'='unavailable' ` +
                 `AND receipt.filesystem_diff IS NULL ` +
                 `AND NULLIF(BTRIM(COALESCE(` +
                   `receipt.receipt->>'unavailableReason','')), '') IS NOT NULL)` +
             `)` +
        `)` +
      `),` +
      `'invalidDeadlineSnapshots',(` +
        `SELECT count(*) FROM latest_cycles cycle WHERE NOT EXISTS (` +
          `SELECT 1 FROM "KothCycleAuditReceipts" receipt ` +
           `WHERE receipt.cycle_id=cycle.id AND receipt.phase='DeadlineSnapshot' ` +
             `AND receipt.attempt=cycle.reset_attempt AND (` +
               `(receipt.receipt->>'status'='captured' ` +
                 `AND jsonb_typeof(receipt.filesystem_diff)='array') OR (` +
               `receipt.receipt->>'status'='unavailable' ` +
                 `AND receipt.filesystem_diff IS NULL ` +
                 `AND NULLIF(BTRIM(COALESCE(` +
                   `receipt.receipt->>'unavailableReason','')), '') IS NOT NULL)` +
             `)` +
        `)` +
      `),` +
      `'invalidTerminalReceipts',(` +
        `SELECT count(*) FROM hills hill ` +
         `LEFT JOIN latest_cycles cycle ON cycle.challenge_id=hill.challenge_id ` +
        `WHERE cycle.id IS NULL OR NOT (` +
          `(cycle.phase='Completed' AND EXISTS (` +
            `SELECT 1 FROM "KothCycleAuditReceipts" receipt ` +
             `WHERE receipt.cycle_id=cycle.id AND receipt.phase='DeadlineCleanup' ` +
               `AND receipt.attempt=cycle.reset_attempt` +
          `)) OR (cycle.phase='Ended' AND EXISTS (` +
            `SELECT 1 FROM "KothCycleAuditReceipts" receipt ` +
             `WHERE receipt.cycle_id=cycle.id AND receipt.phase='Ended' ` +
               `AND receipt.attempt=cycle.reset_attempt` +
          `))` +
        `)` +
      `),` +
      `'unfinalizedTerminalCycles',(` +
        `SELECT count(*) FROM "KothCrownCycles" cycle ` +
         `WHERE cycle.game_id=${gameId} ` +
           `AND cycle.phase IN ('Completed','Ended') ` +
           `AND (cycle.finalized_at IS NULL OR cycle.completed_at IS NULL ` +
             `OR (cycle.actual_start_round IS NOT NULL AND cycle.actual_end_round IS NULL))` +
      `),` +
      `'liveTokens',(` +
        `SELECT count(*) FROM "KothTokens" token ` +
         `JOIN "KothCrownCycles" cycle ON cycle.id=token.cycle_id ` +
        `WHERE cycle.game_id=${gameId} AND token.revoked_at IS NULL` +
      `),` +
      `'claimStates',(` +
        `SELECT count(*) FROM "KothClaimStates" claim ` +
         `JOIN hills hill ON hill.target_id=claim.target_id` +
      `),` +
      `'dirtyTargets',(` +
        `SELECT count(*) FROM "KothTargets" target WHERE target.game_id=${gameId} ` +
          `AND (target.host<>'' OR target.port<>0 OR target.container_id IS NOT NULL ` +
            `OR target.holder_participation_id IS NOT NULL OR target.held_since IS NOT NULL)` +
      `),` +
      `'liveContainerRows',(` +
        `SELECT count(DISTINCT container.id) FROM "Containers" container ` +
        `WHERE container.container_id IN (` +
          `SELECT identity.container_id FROM cycle_containers identity ` +
           `WHERE identity.container_id IS NOT NULL` +
        `) OR container.id IN (` +
          `SELECT challenge.shared_container_id FROM "GameChallenges" challenge ` +
           `JOIN hills hill ON hill.challenge_id=challenge.id ` +
          `WHERE challenge.shared_container_id IS NOT NULL` +
        `)` +
      `),` +
      `'sharedContainerReferences',(` +
        `SELECT count(*) FROM "GameChallenges" challenge ` +
         `JOIN hills hill ON hill.challenge_id=challenge.id ` +
        `WHERE challenge.shared_container_id IS NOT NULL` +
      `),` +
      `'unreleasedCooldowns',(` +
        `SELECT count(*) FROM "KothCycleCooldowns" cooldown ` +
         `JOIN "KothCrownCycles" cycle ON cycle.id=cooldown.cycle_id ` +
        `WHERE cycle.game_id=${gameId} ` +
          `AND cooldown.network_released_at IS NULL` +
      `)` +
    `)::text`
  );
}

export function assessKothDeadlineCleanup(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("KotH deadline cleanup snapshot must be an object");
  }
  const actualFields = Object.keys(value).sort();
  const expectedFields = [...SNAPSHOT_FIELDS].sort();
  if (
    actualFields.length !== expectedFields.length ||
    actualFields.some((field, index) => field !== expectedFields[index])
  ) {
    throw new TypeError("KotH deadline cleanup snapshot fields are incomplete or unexpected");
  }
  const snapshot = Object.fromEntries(
    SNAPSHOT_FIELDS.map((field) => [field, count(value[field], `KotH cleanup ${field}`)]),
  );
  const validTerminalReceipts =
    snapshot.completedCleanupReceipts + snapshot.endedReceipts;
  const failures = Object.freeze({
    "missing KotH hills": snapshot.hillCount > 0 ? 0 : 1,
    "missing terminal KotH cycles":
      snapshot.hillCount === snapshot.latestCycleCount ? 0 : 1,
    "missing or invalid terminal KotH receipts": Math.max(
      snapshot.invalidTerminalReceipts,
      Math.abs(snapshot.hillCount - validTerminalReceipts),
    ),
    "missing or invalid deadline snapshots": Math.max(
      snapshot.invalidDeadlineSnapshots,
      Math.abs(snapshot.hillCount - snapshot.deadlineSnapshotReceipts),
    ),
    "unfinalized terminal KotH cycles": snapshot.unfinalizedTerminalCycles,
    "live KotH tokens after deadline": snapshot.liveTokens,
    "live KotH claim state after deadline": snapshot.claimStates,
    "uncleared KotH target runtime": snapshot.dirtyTargets,
    "live KotH container bookkeeping": snapshot.liveContainerRows,
    "live KotH shared-container references": snapshot.sharedContainerReferences,
    "unreleased KotH cooldowns": snapshot.unreleasedCooldowns,
  });
  return Object.freeze({
    ...snapshot,
    validTerminalReceipts,
    failures,
    converged: Object.values(failures).every((failure) => failure === 0),
  });
}
