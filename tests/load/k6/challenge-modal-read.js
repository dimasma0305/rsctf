// Fixed-rate read-only simulation of challenge modal open/refresh cycles.
import http from "k6/http";
import { Rate, Trend } from "k6/metrics";
import { validSolverPage } from "../challenge-modal-read-model.js";

const TARGET = __ENV.TARGET || "http://127.0.0.1:8080";
const GAME = __ENV.GAME || "";
const CHALLENGE = __ENV.CHALLENGE || "";
const TOKENS = JSON.parse(open(__ENV.TOKENS_FILE || ""));
const RATE = Number(__ENV.RATE || 10);
const VUS = Number(__ENV.VUS || Math.max(10, RATE));

if (!/^\d+$/.test(GAME) || !/^\d+$/.test(CHALLENGE) || TOKENS.length === 0)
  throw new Error(
    "GAME, CHALLENGE, and accepted-participant tokens are required",
  );
if (
  !Number.isSafeInteger(RATE) ||
  RATE <= 0 ||
  !Number.isSafeInteger(VUS) ||
  VUS <= 0
)
  throw new Error("RATE and VUS must be positive integers");

const detailFailures = new Rate("challenge_modal_detail_failure");
const solverFailures = new Rate("challenge_modal_solver_failure");
const server5xx = new Rate("challenge_modal_server_5xx");
const detailDuration = new Trend("challenge_modal_detail_ms", true);
const solverDuration = new Trend("challenge_modal_solver_ms", true);

export const options = {
  scenarios: {
    modalCycles: {
      executor: "constant-arrival-rate",
      rate: RATE,
      timeUnit: "1s",
      duration: __ENV.DURATION || "30s",
      preAllocatedVUs: VUS,
      maxVUs: VUS * 2,
    },
  },
  summaryTrendStats: ["avg", "med", "p(90)", "p(95)", "p(99)", "max"],
  thresholds: {
    challenge_modal_detail_failure: ["rate==0"],
    challenge_modal_solver_failure: ["rate==0"],
    challenge_modal_server_5xx: ["rate==0"],
    challenge_modal_detail_ms: ["p(95)<800"],
    challenge_modal_solver_ms: ["p(95)<800"],
    dropped_iterations: ["count==0"],
  },
};

function sourceIp(index) {
  return `31.${1 + (index % 240)}.${1 + (Math.floor(index / 240) % 250)}.${1 + (index % 250)}`;
}

export default function () {
  const tokenIndex = ((__VU - 1) * 997 + __ITER) % TOKENS.length;
  const params = {
    headers: {
      Authorization: `Bearer ${TOKENS[tokenIndex]}`,
      "X-Real-IP": sourceIp(tokenIndex),
      Accept: "application/json",
    },
  };
  const responses = http.batch([
    [
      "GET",
      `${TARGET}/api/game/${GAME}/challenges/${CHALLENGE}`,
      null,
      { ...params, tags: { endpoint: "challenge_modal_detail" } },
    ],
    [
      "GET",
      `${TARGET}/api/game/${GAME}/challenges/${CHALLENGE}/solvers/page?count=20&skip=0`,
      null,
      { ...params, tags: { endpoint: "challenge_modal_solvers" } },
    ],
  ]);
  const [detail, solvers] = responses;
  detailDuration.add(detail.timings.duration);
  solverDuration.add(solvers.timings.duration);
  server5xx.add(detail.status >= 500 || solvers.status >= 500);

  let detailModel = null;
  let solverModel = null;
  try {
    detailModel = detail.json();
  } catch (_) {}
  try {
    solverModel = solvers.json();
  } catch (_) {}
  detailFailures.add(
    detail.status !== 200 ||
      !detailModel ||
      typeof detailModel !== "object" ||
      typeof detailModel.title !== "string",
  );
  solverFailures.add(
    solvers.status !== 200 ||
      !validSolverPage(solverModel, solvers.body?.length ?? 0),
  );
}
