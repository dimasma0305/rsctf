import assert from "node:assert/strict";
import test from "node:test";

import { ensureValidTeamToken } from "../team-token-readiness.js";

test("a token rotated during cohort setup is replaced and revalidated", async () => {
  const probes = [];
  let rotations = 0;
  const token = await ensureValidTeamToken({
    token: "stale",
    probe: async (candidate) => {
      probes.push(candidate);
      return { status: candidate === "fresh" ? 200 : 401 };
    },
    rotate: async () => {
      rotations++;
      return "fresh";
    },
  });

  assert.equal(token, "fresh");
  assert.equal(rotations, 1);
  assert.deepEqual(probes, ["stale", "fresh"]);
});

test("a valid token is retained without another rotation", async () => {
  let rotations = 0;
  const token = await ensureValidTeamToken({
    token: "valid",
    probe: async () => ({ status: 200 }),
    rotate: async () => {
      rotations++;
      return "unused";
    },
  });
  assert.equal(token, "valid");
  assert.equal(rotations, 0);
});

test("transient probe failures retry without rotating the credential", async () => {
  const statuses = [503, 429, 200];
  const waits = [];
  const token = await ensureValidTeamToken({
    token: "valid",
    probe: async () => ({ status: statuses.shift() }),
    rotate: async () =>
      assert.fail("transient failures must not rotate the token"),
    wait: async (milliseconds) => waits.push(milliseconds),
  });
  assert.equal(token, "valid");
  assert.deepEqual(waits, [500, 1000]);
});

test("terminal probe failures stop without exposing the credential", async () => {
  await assert.rejects(
    ensureValidTeamToken({
      token: "secret-token",
      probe: async () => ({ status: 404 }),
      rotate: async () => "unused",
    }),
    (error) =>
      !error.message.includes("secret-token") && /HTTP 404/.test(error.message),
  );
});

test("persistent authorization failure rotates once and then fails closed", async () => {
  let rotations = 0;
  await assert.rejects(
    ensureValidTeamToken({
      token: "stale",
      probe: async () => ({ status: 401 }),
      rotate: async () => {
        rotations++;
        return "fresh";
      },
    }),
    /remained unauthorized/,
  );
  assert.equal(rotations, 1);
});

test("the final attempt never performs an unverifiable rotation", async () => {
  let rotations = 0;
  await assert.rejects(
    ensureValidTeamToken({
      token: "stale",
      probe: async () => ({ status: 401 }),
      rotate: async () => {
        rotations++;
        return "fresh";
      },
      maxAttempts: 1,
    }),
    /remained unauthorized/,
  );
  assert.equal(rotations, 0);
});
