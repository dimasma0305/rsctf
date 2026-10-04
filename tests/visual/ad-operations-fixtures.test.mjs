import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fixture } from "./ad-operations-fixtures.mjs";

test("importing operator fixtures never starts a server, even with a caller --serve flag", () => {
  const result = spawnSync(
    process.execPath,
    [
      "--input-type=module",
      "--eval",
      `await import(${JSON.stringify(new URL("./ad-operations-fixtures.mjs", import.meta.url).href)})`,
      "--",
      "--serve",
    ],
    { encoding: "utf8", timeout: 3000 },
  );
  assert.equal(result.error, undefined);
  assert.equal(result.status, 0, result.stderr);
});

test("operator fixtures reject writes, non-admin reads, and unknown endpoints", () => {
  const path = "/api/edit/games/19/ad/state";
  assert.equal(fixture(path, "POST").status, 405);
  assert.equal(fixture(path, "GET", "normal", "User").status, 403);
  assert.equal(fixture("/api/not-a-fixture").status, 404);
  assert.equal(fixture(path, "GET", "stale").status, 503);
  assert.equal(fixture(path, "GET", "loading").hold, true);
  assert.ok(
    fixture(path).body.teams.some((team) => team.services.length === 0),
  );
});
