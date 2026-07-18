import assert from "node:assert/strict";
import test from "node:test";

import {
  assessKothDeadlineCleanup,
  kothDeadlineCleanupQuery,
} from "../koth-deadline-cleanup.js";

const completedClean = Object.freeze({
  hillCount: 1,
  latestCycleCount: 1,
  completedCleanupReceipts: 1,
  endedReceipts: 0,
  deadlineSnapshotReceipts: 1,
  invalidDeadlineSnapshots: 0,
  invalidTerminalReceipts: 0,
  unfinalizedTerminalCycles: 0,
  liveTokens: 0,
  claimStates: 0,
  dirtyTargets: 0,
  liveContainerRows: 0,
  sharedContainerReferences: 0,
  unreleasedCooldowns: 0,
});

test("deadline cleanup query binds every runtime leak to one validated game", () => {
  const query = kothDeadlineCleanupQuery(44);
  assert.match(query, /target\.game_id=44/);
  assert.match(query, /cycle\.phase='Completed'/);
  assert.match(query, /receipt\.phase='DeadlineCleanup'/);
  assert.match(query, /cycle\.phase='Ended'/);
  assert.match(query, /receipt\.phase='Ended'/);
  assert.match(query, /receipt\.phase='DeadlineSnapshot'/);
  assert.match(query, /receipt\.receipt->>'status'='captured'/);
  assert.match(query, /receipt\.receipt->>'status'='unavailable'/);
  assert.match(query, /jsonb_typeof\(receipt\.filesystem_diff\)='array'/);
  assert.match(query, /unavailableReason/);
  assert.match(query, /receipt\.attempt=cycle\.reset_attempt/);
  assert.match(query, /token\.revoked_at IS NULL/);
  assert.match(query, /"KothClaimStates"/);
  assert.match(query, /target\.container_id IS NOT NULL/);
  assert.match(query, /"Containers"/);
  assert.match(query, /shared_container_id IS NOT NULL/);
  assert.match(query, /network_released_at IS NULL/);
  assert.doesNotMatch(query, /cooldown\.network_enforced=TRUE/);
  assert.doesNotMatch(query, /\b(?:UPDATE|DELETE|INSERT)\b/i);
  assert.throws(
    () => kothDeadlineCleanupQuery("44; DROP TABLE"),
    /positive game id/,
  );
});

test("post-deadline cleanup converges only after every durable blocker clears", () => {
  const endedClean = {
    ...completedClean,
    completedCleanupReceipts: 0,
    endedReceipts: 1,
  };
  assert.equal(assessKothDeadlineCleanup(completedClean).converged, true);
  assert.equal(assessKothDeadlineCleanup(endedClean).converged, true);
  for (const field of [
    "invalidTerminalReceipts",
    "invalidDeadlineSnapshots",
    "unfinalizedTerminalCycles",
    "liveTokens",
    "claimStates",
    "dirtyTargets",
    "liveContainerRows",
    "sharedContainerReferences",
    "unreleasedCooldowns",
  ]) {
    const assessment = assessKothDeadlineCleanup({
      ...completedClean,
      [field]: 1,
    });
    assert.equal(assessment.converged, false, `${field} must block settlement`);
  }
  assert.equal(
    assessKothDeadlineCleanup({
      ...completedClean,
      completedCleanupReceipts: 0,
    }).converged,
    false,
  );
  assert.equal(
    assessKothDeadlineCleanup({ ...completedClean, endedReceipts: 1 })
      .converged,
    false,
  );
  assert.equal(
    assessKothDeadlineCleanup({
      ...completedClean,
      deadlineSnapshotReceipts: 0,
    }).converged,
    false,
  );
  assert.equal(
    assessKothDeadlineCleanup({
      ...completedClean,
      hillCount: 0,
      latestCycleCount: 0,
      completedCleanupReceipts: 0,
    }).converged,
    false,
  );
});

test("cleanup snapshot rejects missing, extra, negative, and fractional counts", () => {
  const missing = { ...completedClean };
  delete missing.liveTokens;
  assert.throws(
    () => assessKothDeadlineCleanup(missing),
    /fields are incomplete/,
  );
  assert.throws(
    () => assessKothDeadlineCleanup({ ...completedClean, surprise: 0 }),
    /unexpected/,
  );
  assert.throws(
    () => assessKothDeadlineCleanup({ ...completedClean, claimStates: -1 }),
    /safe integer/,
  );
  assert.throws(
    () => assessKothDeadlineCleanup({ ...completedClean, liveTokens: 0.5 }),
    /safe integer/,
  );
});
