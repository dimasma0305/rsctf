import execution from "k6/execution";
import http from "k6/http";
import { check } from "k6";
import { Counter, Trend } from "k6/metrics";

const CONFIG_PATH = __ENV.CHEAT_CONFIG;
if (!CONFIG_PATH) throw new Error("CHEAT_CONFIG is required");
const C = JSON.parse(open(CONFIG_PATH));

const server5xx = new Counter("server_5xx");
const unexpectedResponses = new Counter("unexpected_responses");
const stolenSubmissions = new Counter("stolen_submissions");
const bruteAttempts = new Counter("brute_attempts");
const bruteSubmissions = new Counter("brute_submissions");
const bruteRateLimited = new Counter("brute_rate_limited");
const honeypotHits = new Counter("honeypot_hits");
const cleanPolls = new Counter("clean_control_polls");
const requestMs = new Trend("cheat_sim_request_ms", true);

function scenario(name, config) {
  return {
    executor: "per-vu-iterations",
    exec: name,
    vus: config.vus,
    iterations: config.iterations,
    maxDuration: "2m",
    startTime: config.startTime,
  };
}

export const options = {
  scenarios: {
    cleanControls: scenario("cleanControl", {
      vus: C.clean.length,
      iterations: 1,
      startTime: "0s",
    }),
    stolenFlags: scenario("stolenFlag", {
      vus: C.stolen.length,
      iterations: 1,
      startTime: "1s",
    }),
    bruteForce: scenario("bruteForce", {
      vus: C.brute.tokens.length,
      iterations: C.brute.attemptsPerToken,
      startTime: "2s",
    }),
    scanner: scenario("scanner", { vus: 1, iterations: 1, startTime: "2s" }),
  },
  thresholds: {
    server_5xx: ["count==0"],
    unexpected_responses: ["count==0"],
    stolen_submissions: [`count==${C.stolen.length}`],
    brute_attempts: [
      `count==${C.brute.tokens.length * C.brute.attemptsPerToken}`,
    ],
    brute_submissions: [
      `count==${C.brute.tokens.length * C.brute.attemptsPerToken}`,
    ],
    honeypot_hits: [`count==${C.honeypot.baits.length}`],
    clean_control_polls: [`count==${C.clean.length * 3}`],
  },
  summaryTrendStats: ["avg", "med", "p(90)", "p(95)", "p(99)", "max"],
};

function params(actor, tags = {}) {
  return {
    headers: {
      Authorization: `Bearer ${actor.jwt}`,
      "Content-Type": "application/json",
      Origin: C.origin,
      "X-Real-IP": actor.ip,
    },
    tags,
    timeout: "15s",
  };
}

function record(response, expected, label) {
  requestMs.add(response.timings.duration, { operation: label });
  if (response.status >= 500) server5xx.add(1, { operation: label });
  const valid = expected.includes(response.status);
  if (!valid)
    unexpectedResponses.add(1, {
      operation: label,
      status: String(response.status),
    });
  check(response, { [`${label}: expected status`]: () => valid });
  return valid;
}

function submit(actor, flag, label) {
  return http.post(
    `${C.target}/api/game/${C.gameId}/challenges/${C.challengeId}`,
    JSON.stringify({ flag }),
    params(actor, { operation: label }),
  );
}

export function stolenFlag() {
  const actor = C.stolen[execution.scenario.iterationInTest % C.stolen.length];
  const response = submit(actor, actor.victimFlag, "stolen_flag");
  if (record(response, [200], "stolen flag submission"))
    stolenSubmissions.add(1);
}

export function bruteForce() {
  const tokenIndex = execution.scenario.iterationInTest % C.brute.tokens.length;
  const actor = C.brute.tokens[tokenIndex];
  const attempt = execution.scenario.iterationInTest;
  const response = submit(
    actor,
    `flag{invalid_${C.runId}_${tokenIndex}_${attempt}}`,
    "brute_force",
  );
  bruteAttempts.add(1);
  if (
    record(response, [200, 429], "brute-force submission") &&
    response.status === 200
  )
    bruteSubmissions.add(1);
  if (response.status === 429) bruteRateLimited.add(1);
}

export function scanner() {
  for (const bait of C.honeypot.baits) {
    const response = http.get(
      `${C.target}${bait}`,
      params(C.honeypot, { operation: "honeypot" }),
    );
    if (record(response, [404], "honeypot probe")) honeypotHits.add(1);
  }
}

export function cleanControl() {
  const actor = C.clean[execution.scenario.iterationInTest % C.clean.length];
  const endpoints = [
    `/api/Game/${C.gameId}/Ad/State`,
    `/api/Game/${C.gameId}/Ad/Scoreboard`,
    `/api/game/${C.gameId}/ad/koth/scoreboard`,
  ];
  for (const endpoint of endpoints) {
    const response = http.get(
      `${C.target}${endpoint}`,
      params(actor, { operation: "clean_control" }),
    );
    if (record(response, [200], "clean control poll")) cleanPolls.add(1);
  }
}
