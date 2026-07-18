function positiveIdentifier(value, label) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 1) {
    throw new TypeError(`${label} must be a positive safe integer`);
  }
  return parsed;
}

// A player can prove that its patched runtime disappeared by observing a new
// instance identity. Pristine same-image recreation is instead an operator-side
// invariant: validate the durable destroy, create, and readiness receipt chain
// for every cycle activated during this lifecycle run. Crash-backfilled receipts
// intentionally contain only recovered/durable identity fields; detailed normal
// receipts must additionally bind the expected image and functional Ok verdict.
export function kothResetReceiptIntegrityQuery(gameValue, firstCycleValue) {
  const gameId = positiveIdentifier(gameValue, "KotH reset receipt game id");
  const firstCycleId = positiveIdentifier(
    firstCycleValue,
    "KotH reset receipt first cycle id",
  );

  return (
    `WITH activated_cycles AS (` +
      `SELECT cycle.id,cycle.reset_attempt,cycle.old_container_id,` +
        `cycle.replacement_container_id,cycle.expected_image ` +
        `FROM "KothCrownCycles" cycle ` +
       `WHERE cycle.game_id=${gameId} ` +
         `AND cycle.id>=${firstCycleId} AND cycle.actual_start_round IS NOT NULL` +
    `) SELECT count(*) FROM activated_cycles cycle WHERE ` +
      `NULLIF(BTRIM(cycle.old_container_id),'') IS NULL ` +
      `OR NULLIF(BTRIM(cycle.replacement_container_id),'') IS NULL ` +
      `OR cycle.old_container_id=cycle.replacement_container_id ` +
      `OR NULLIF(BTRIM(cycle.expected_image),'') IS NULL ` +
      `OR NOT EXISTS (` +
        `SELECT 1 FROM "KothCycleAuditReceipts" receipt ` +
         `WHERE receipt.cycle_id=cycle.id AND receipt.phase='DestroyPending' ` +
           `AND receipt.attempt=cycle.reset_attempt AND (` +
             `receipt.receipt->>'destroyedContainerId'=cycle.old_container_id OR (` +
               `receipt.receipt->>'recovered'='true' ` +
               `AND receipt.receipt->>'oldContainerId'=cycle.old_container_id` +
             `)` +
           `)` +
      `) OR NOT EXISTS (` +
        `SELECT 1 FROM "KothCycleAuditReceipts" receipt ` +
         `WHERE receipt.cycle_id=cycle.id AND receipt.phase='CreatePending' ` +
           `AND receipt.attempt=cycle.reset_attempt AND ((` +
             `receipt.receipt->>'replacementContainerId'=cycle.replacement_container_id ` +
             `AND receipt.receipt->>'image'=cycle.expected_image` +
           `) OR (` +
             `receipt.receipt->>'recovered'='true' ` +
             `AND receipt.receipt->>'replacementContainerId'=cycle.replacement_container_id` +
           `))` +
      `) OR NOT EXISTS (` +
        `SELECT 1 FROM "KothCycleAuditReceipts" receipt ` +
         `WHERE receipt.cycle_id=cycle.id AND receipt.phase='ReadinessPending' ` +
           `AND receipt.attempt=cycle.reset_attempt AND ((` +
             `receipt.receipt->>'containerId'=cycle.replacement_container_id ` +
             `AND receipt.receipt->>'functionalStatus'='Ok'` +
           `) OR (` +
             `receipt.receipt->>'recovered'='true' ` +
             `AND receipt.receipt->>'replacementContainerId'=cycle.replacement_container_id ` +
             `AND receipt.receipt->>'durablePhase' IN (` +
               `'FirewallPending','Active','Completed'` +
             `)` +
           `))` +
      `)`
  );
}
