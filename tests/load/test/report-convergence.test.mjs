import assert from "node:assert/strict";
import { test } from "node:test";

import { loadAuthoritativeAfterConcurrentSweep } from "../report-convergence.js";

test("authoritative report is loaded after the concurrent sweep converges", async () => {
  const persisted = [];
  const delays = [5, 15, 25];
  let active = 0;
  let maximumActive = 0;
  const fetchReport = async (index) => {
    active++;
    maximumActive = Math.max(maximumActive, active);
    if (index < delays.length) {
      await new Promise((resolve) => setTimeout(resolve, delays[index]));
      persisted.push(index);
    }
    const body = [...persisted];
    active--;
    return body;
  };

  const { sweep, authoritative } = await loadAuthoritativeAfterConcurrentSweep(
    fetchReport,
    3,
  );

  assert.equal(maximumActive, 3);
  assert.deepEqual(sweep[0], [0]);
  assert.deepEqual(sweep[2], [0, 1, 2]);
  assert.deepEqual(authoritative, [0, 1, 2]);
});
