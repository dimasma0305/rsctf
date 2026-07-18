import assert from "node:assert/strict";
import test from "node:test";

import { kothResetReceiptIntegrityQuery } from "../koth-reset-receipts.js";

test("reset receipt integrity query is run-scoped and validates the durable chain", () => {
  const query = kothResetReceiptIntegrityQuery(114, 2040);

  assert.match(query, /cycle\.game_id=114/);
  assert.match(query, /cycle\.id>=2040/);
  assert.match(query, /cycle\.actual_start_round IS NOT NULL/);
  assert.match(query, /cycle\.old_container_id=cycle\.replacement_container_id/);
  assert.match(query, /receipt\.phase='DestroyPending'/);
  assert.match(query, /destroyedContainerId/);
  assert.match(query, /receipt\.phase='CreatePending'/);
  assert.match(query, /replacementContainerId/);
  assert.match(query, /receipt\.receipt->>'image'=cycle\.expected_image/);
  assert.match(query, /receipt\.phase='ReadinessPending'/);
  assert.match(query, /receipt\.receipt->>'functionalStatus'='Ok'/);
  assert.match(query, /receipt\.attempt=cycle\.reset_attempt/);
  assert.match(query, /receipt\.receipt->>'recovered'='true'/);
  assert.doesNotMatch(query, /\b(?:UPDATE|DELETE|INSERT)\b/i);
});

test("reset receipt integrity query rejects unsafe identities", () => {
  assert.throws(
    () => kothResetReceiptIntegrityQuery("114; DROP TABLE", 2040),
    /game id must be a positive safe integer/,
  );
  assert.throws(
    () => kothResetReceiptIntegrityQuery(114, 0),
    /first cycle id must be a positive safe integer/,
  );
  assert.throws(
    () => kothResetReceiptIntegrityQuery(114, 1.5),
    /first cycle id must be a positive safe integer/,
  );
});
