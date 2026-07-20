import assert from "node:assert/strict";
import test from "node:test";

import {
  buildLifecycleFleet,
  lifecycleFleetIp,
  lifecycleFleetSlot,
  lifecycleFleetSlots,
  reserveLifecycleContainerUsers,
  retryAfterDelaySeconds,
  selectKothCapacityClaimant,
  shouldValidateSemanticResponse,
} from "../lifecycle-load-model.js";

const state = {
  adUsers: ["user-a", "user-b", "user-c", "user-d"],
  adPartIds: [101, 102, 103, 104],
  // Deliberately unordered: SQL does not promise the flag snapshot's row order.
  plantedFlags: [
    { pid: 104, flag: "flag-d" },
    { pid: 102, flag: "flag-b" },
    { pid: 101, flag: "flag-a" },
    { pid: 103, flag: "flag-c" },
  ],
};

test("scenario iterations cover every fleet identity independently of global VU ids", () => {
  assert.deepEqual(
    Array.from({ length: 8 }, (_, iteration) =>
      lifecycleFleetSlot(iteration, 4),
    ),
    [0, 1, 2, 3, 0, 1, 2, 3],
  );
  assert.deepEqual(lifecycleFleetSlots(6, 4), [0, 1, 2, 3, 0, 1]);
});

test("capacity KotH claimants stay in the VPN-backed fleet and skip cooldowns", () => {
  const fleet = [101, 102, 103, 104];
  assert.equal(selectKothCapacityClaimant(fleet, new Set(), 1), 101);
  assert.equal(selectKothCapacityClaimant(fleet, new Set(), 2), 102);
  assert.equal(selectKothCapacityClaimant(fleet, new Set([101, 102]), 2), 104);
  assert.equal(selectKothCapacityClaimant(fleet, new Set(fleet), 3), null);
  assert.throws(
    () => selectKothCapacityClaimant([101, 101], new Set(), 1),
    /must be distinct/,
  );
});

test("semantic integrity samples exclude expected rate-limit responses", () => {
  assert.equal(shouldValidateSemanticResponse(200), true);
  assert.equal(shouldValidateSemanticResponse(400), true);
  assert.equal(shouldValidateSemanticResponse(500), true);
  assert.equal(shouldValidateSemanticResponse(429), false);
});

test("container lifecycle identities are reserved away from Jeopardy polling", () => {
  assert.deepEqual(
    reserveLifecycleContainerUsers(
      ["player-a", "player-b", "container-a", "container-b"],
      2,
    ),
    {
      playerUsers: ["player-a", "player-b"],
      containerUsers: ["container-a", "container-b"],
    },
  );
  assert.deepEqual(
    reserveLifecycleContainerUsers(["player-a", "player-b"], 0),
    { playerUsers: ["player-a", "player-b"], containerUsers: [] },
  );
  assert.throws(
    () => reserveLifecycleContainerUsers(["only-player"], 1),
    /leave at least one Jeopardy player/,
  );
  assert.throws(
    () => reserveLifecycleContainerUsers(["duplicate", "duplicate"], 1),
    /must be distinct/,
  );
});

test("container teardown retries honor delta-seconds and HTTP-date Retry-After", () => {
  const now = Date.parse("2026-07-20T00:00:00Z");
  assert.equal(retryAfterDelaySeconds("10", 1, 60, now), 10);
  assert.equal(
    retryAfterDelaySeconds("Sun, 20 Jul 2026 00:00:12 GMT", 1, 60, now),
    12,
  );
  assert.equal(retryAfterDelaySeconds("invalid", 1.5, 60, now), 1.5);
  assert.equal(retryAfterDelaySeconds("120", 1, 60, now), 60);
  assert.equal(retryAfterDelaySeconds("0", 1, 60, now), 0.1);
});

test("fleet identities bind users to exact in-cohort victims and unordered flags", () => {
  assert.deepEqual(buildLifecycleFleet(state, 3), [
    {
      index: 0,
      participationId: 101,
      userId: "user-a",
      victimParticipationId: 102,
      victimFlag: "flag-b",
    },
    {
      index: 1,
      participationId: 102,
      userId: "user-b",
      victimParticipationId: 103,
      victimFlag: "flag-c",
    },
    {
      index: 2,
      participationId: 103,
      userId: "user-c",
      victimParticipationId: 101,
      victimFlag: "flag-a",
    },
  ]);
  assert.equal(lifecycleFleetIp(0), "10.240.0.1");
  assert.equal(lifecycleFleetIp(254), "10.240.1.1");
});

test("fleet construction rejects incomplete or ambiguous evidence", () => {
  assert.throws(() => buildLifecycleFleet(state, 5), /for a 5-team fleet/);
  assert.throws(
    () => buildLifecycleFleet({ ...state, adPartIds: [101, 101, 103, 104] }, 4),
    /must be distinct/,
  );
  assert.throws(
    () =>
      buildLifecycleFleet(
        { ...state, plantedFlags: state.plantedFlags.slice(1) },
        4,
      ),
    /missing planted flag/,
  );
});
