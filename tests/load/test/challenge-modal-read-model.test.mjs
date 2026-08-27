import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
  MAX_MODAL_SOLVER_BODY_BYTES,
  validSolverPage,
} from "../challenge-modal-read-model.js";

const solver = (number) => ({
  teamName: `Team ${number}`,
  teamAvatar: null,
  userName: null,
  type: "Normal",
  time: 1_777_000_000_000 + number,
});

test("maximum-roster solver responses expose only one bounded visible page", () => {
  const page = {
    data: Array.from({ length: 20 }, (_, index) => solver(index + 1)),
    total: 500,
    nextSkip: 20,
  };
  assert.equal(validSolverPage(page, 8_192), true);
  assert.equal(
    validSolverPage({ ...page, data: [...page.data, solver(21)] }, 8_192),
    false,
  );
  assert.equal(validSolverPage(page, MAX_MODAL_SOLVER_BODY_BYTES + 1), false);
});

test("solver page validation rejects malformed pagination and HTML masquerading as data", () => {
  assert.equal(validSolverPage("<!doctype html>", 15), false);
  assert.equal(
    validSolverPage({ data: [], total: -1, nextSkip: null }, 40),
    false,
  );
  assert.equal(
    validSolverPage({ data: [solver(1)], total: 1, nextSkip: 0 }, 200),
    false,
  );
  assert.equal(
    validSolverPage({ data: [solver(1)], total: 1, nextSkip: 2 }, 200),
    false,
  );
});

test("fixed-rate modal load keeps one two-request batch for every roster size", () => {
  const scenario = readFileSync("k6/challenge-modal-read.js", "utf8");
  assert.match(scenario, /executor: "constant-arrival-rate"/);
  assert.match(scenario, /const responses = http\.batch\(\[/);
  assert.equal(
    [...scenario.matchAll(/endpoint: "challenge_modal_(?:detail|solvers)"/g)]
      .length,
    2,
  );
  assert.match(scenario, /solvers\/page\?count=20&skip=0/);
  assert.doesNotMatch(
    scenario,
    /challenges\/\$\{CHALLENGE\}\/solvers`,/,
  );
  assert.match(scenario, /dropped_iterations: \["count==0"\]/);
});
