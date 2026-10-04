import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  MAX_PARTICIPATION_REVIEW_PAGE_BYTES,
  participationReviewOperations,
  validParticipationReviewDetail,
  validParticipationReviewPage,
} from "../participation-review.js";

const summary = (id) => ({
  id,
  teamId: id + 100,
  teamName: `Team ${id}`,
  teamAvatar: null,
  registeredMemberCount: 1,
  teamMemberCount: 2,
  divisionId: 4,
  status: "Accepted",
});
const member = {
  userId: "00000000-0000-0000-0000-000000000001",
  userName: "captain",
  email: "captain@example.test",
  realName: null,
  stdNumber: null,
  phone: null,
  avatar: null,
  isRegistered: true,
  isCaptain: true,
};

test("participation review pages stay bounded and summaries admit no PII fields", () => {
  const page = { data: Array.from({ length: 50 }, (_, index) => summary(index + 1)), total: 12_000, length: 50 };
  assert.equal(validParticipationReviewPage(page, 32_000, 50), true);
  assert.equal(validParticipationReviewPage({ ...page, data: [...page.data, summary(51)], length: 51 }, 33_000, 50), false);
  assert.equal(validParticipationReviewPage({ ...page, data: [{ ...summary(1), email: "leak@example.test" }], length: 1 }, 500, 50), false);
  assert.equal(validParticipationReviewPage(page, MAX_PARTICIPATION_REVIEW_PAGE_BYTES + 1, 50), false);
  assert.equal(validParticipationReviewPage({ ...page, length: 49 }, 32_000, 50), false);
});

test("one lazy roster detail accepts only the declared member projection", () => {
  const detail = { id: 7, teamId: 9, teamName: "Team", teamAvatar: null, members: [member] };
  assert.equal(validParticipationReviewDetail(detail, 2_000), true);
  assert.equal(validParticipationReviewDetail({ ...detail, members: [{ ...member, securityStamp: "secret" }] }, 2_000), false);
  assert.equal(validParticipationReviewDetail({ ...detail, passwordHash: "secret" }, 2_000), false);
});

test("fixed-rate scenario spans page bounds, literal search, filters, lazy detail, and health", () => {
  const operations = participationReviewOperations(17, 23, 4);
  assert.deepEqual(operations.map(({ id }) => id), [
    "page_default",
    "page_max",
    "page_tail",
    "page_status",
    "page_division",
    "page_literal_search",
    "detail",
  ]);

  const scenario = readFileSync(new URL("../k6/participation-review.js", import.meta.url), "utf8");
  const runner = readFileSync(new URL("../participation-review.mjs", import.meta.url), "utf8");
  const routes = readFileSync(new URL("../../../src/controllers/game/routes.rs", import.meta.url), "utf8");
  assert.match(scenario, /executor: "constant-arrival-rate"/);
  assert.equal((scenario.match(/http\.get\(/g) || []).length, 2, "one review read plus one independent health read");
  assert.match(scenario, /dropped_iterations: \["count==0"\]/);
  assert.match(scenario, /participation_review_rate_limited: \["rate==0"\]/);
  assert.match(scenario, /includes\("private, no-store"\)/);
  assert.match(scenario, /response\.headers\.Pragma/);
  assert.match(runner, /participationCount < 12_000/);
  assert.match(runner, /writeFileSync\(tokenFile, JSON\.stringify\(token\), \{ mode: 0o600 \}\)/);
  assert.match(runner, /rmSync\(tokenDirectory, \{ recursive: true, force: true \}\)/);
  assert.doesNotMatch(runner, /\b(?:INSERT|UPDATE|DELETE)\b/);
  for (const route of [
    '"/api/game/{id}/participations"',
    '"/api/game/{id}/participations/{participationId}"',
  ]) {
    const start = routes.indexOf(route);
    assert.notEqual(start, -1, `missing ${route}`);
    assert.match(routes.slice(start, start + 180), /limited\(Policy::Query/);
  }
});
