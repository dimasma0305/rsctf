import assert from "node:assert/strict";
import test from "node:test";

import { cohortSeedQuery, parseCohortSeedResult } from "../cohort-seed.js";

const users = [
  "3d06bc07-6f13-4c89-8f3f-07876974c512",
  "026a573d-72a8-4497-ae25-1aa0945d34b4",
  "e9b0f8fb-a9b6-43bc-a935-d97d953148d9",
];

test("cohort seed is one atomic statement with database-owned identity mapping", () => {
  const query = cohortSeedQuery(77, 100);
  assert.match(query, /^WITH cohort AS MATERIALIZED/);
  assert.equal((query.match(/INSERT INTO/g) || []).length, 5);
  assert.match(query, /FROM generate_series\(1,100\)/);
  assert.match(query, /'LT77_' \|\| cohort\.ordinal/);
  assert.match(query, /'ordinal',cohort\.ordinal/);
  assert.match(query, /ORDER BY cohort\.ordinal/);
  assert.doesNotMatch(query, /BEGIN|COMMIT/);
});

test("cohort result follows ordinals rather than incidental RETURNING order", () => {
  const output = JSON.stringify([
    { ordinal: 3, userId: users[2], teamId: 31, partId: 301 },
    { ordinal: 1, userId: users[0], teamId: 11, partId: 101 },
    { ordinal: 2, userId: users[1], teamId: 21, partId: 201 },
  ]);
  assert.deepEqual(parseCohortSeedResult(output, 3), {
    userIds: users,
    teamIds: [11, 21, 31],
    partIds: [101, 201, 301],
  });
});

test("cohort identity validation rejects partial, duplicate, or malformed results", () => {
  assert.throws(() => parseCohortSeedResult("not-json", 1), /malformed JSON/);
  assert.throws(() => parseCohortSeedResult("[]", 2), /expected 2/);
  assert.throws(
    () =>
      parseCohortSeedResult(
        JSON.stringify([
          { ordinal: 1, userId: users[0], teamId: 11, partId: 101 },
          { ordinal: 2, userId: users[1], teamId: 11, partId: 201 },
        ]),
        2,
      ),
    /duplicate identities/,
  );
  assert.throws(
    () =>
      parseCohortSeedResult(
        JSON.stringify([
          { ordinal: 2, userId: users[0], teamId: 1, partId: 1 },
        ]),
        1,
      ),
    /invalid identity/,
  );
});

test("cohort query rejects non-integer SQL inputs", () => {
  for (const [gameId, count] of [
    ['77; DELETE FROM "Games"', 100],
    [77, 0],
    [77, 1.5],
  ]) {
    assert.throws(() => cohortSeedQuery(gameId, count), /positive integer/);
  }
});
