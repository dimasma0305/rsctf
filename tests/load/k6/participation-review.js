import http from "k6/http";
import { Rate, Trend } from "k6/metrics";

import {
  participationReviewOperations,
  validParticipationReviewResponse,
} from "../participation-review.js";

const TARGET = String(__ENV.TARGET || "http://127.0.0.1:8080").replace(/\/+$/, "");
const GAME = Number(__ENV.GAME);
const PARTICIPATION = Number(__ENV.PARTICIPATION);
const DIVISION = __ENV.DIVISION ? Number(__ENV.DIVISION) : null;
const RATE = Number(__ENV.RATE || 2);
const VUS = Number(__ENV.VUS || Math.max(4, RATE * 2));
const tokenFile = String(__ENV.PARTICIPATION_REVIEW_TOKEN_FILE || "");
const TOKEN = tokenFile ? JSON.parse(open(tokenFile)) : "";
const OPERATIONS = participationReviewOperations(GAME, PARTICIPATION, DIVISION);

if (!TOKEN || typeof TOKEN !== "string") throw new Error("one protected participation review token is required");
if (!Number.isSafeInteger(RATE) || RATE < 1 || RATE > 4) throw new Error("RATE must be an integer from 1 through 4");
if (!Number.isSafeInteger(VUS) || VUS < 1) throw new Error("VUS must be a positive integer");

http.setResponseCallback(http.expectedStatuses(200));

const server5xx = new Rate("participation_review_server_5xx");
const non200 = new Rate("participation_review_non_200");
const rateLimited = new Rate("participation_review_rate_limited");
const invalidBody = new Rate("participation_review_invalid_body");
const healthFailure = new Rate("participation_review_health_failure");
const readMs = new Trend("participation_review_read_ms", true);
const healthMs = new Trend("participation_review_health_ms", true);

export const options = {
  scenarios: {
    reviewReads: {
      executor: "constant-arrival-rate",
      exec: "reviewReads",
      rate: RATE,
      timeUnit: "1s",
      duration: __ENV.DURATION || "30s",
      preAllocatedVUs: VUS,
      maxVUs: VUS * 2,
    },
    platformHealth: {
      executor: "constant-arrival-rate",
      exec: "platformHealth",
      rate: 1,
      timeUnit: "1s",
      duration: __ENV.DURATION || "30s",
      preAllocatedVUs: 2,
      maxVUs: 4,
    },
  },
  summaryTrendStats: ["avg", "med", "p(90)", "p(95)", "p(99)", "max"],
  thresholds: {
    http_req_failed: ["rate==0"],
    participation_review_server_5xx: ["rate==0"],
    participation_review_non_200: ["rate==0"],
    participation_review_rate_limited: ["rate==0"],
    participation_review_invalid_body: ["rate==0"],
    participation_review_health_failure: ["rate==0"],
    dropped_iterations: ["count==0"],
    participation_review_read_ms: [`p(95)<${Number(__ENV.MAX_P95_MS || 1000)}`],
    participation_review_health_ms: ["p(95)<500"],
  },
};

export function reviewReads() {
  const operation = OPERATIONS[((__VU - 1) * 997 + __ITER) % OPERATIONS.length];
  const response = http.get(`${TARGET}${operation.path}`, {
    headers: { Authorization: `Bearer ${TOKEN}` },
    tags: { endpoint: operation.id },
  });
  readMs.add(response.timings.duration);
  server5xx.add(response.status >= 500);
  non200.add(response.status !== 200);
  rateLimited.add(response.status === 429);
  let model;
  try {
    model = response.json();
  } catch (_) {
    model = null;
  }
  const privateDetail =
    operation.kind !== "detail" ||
    (String(response.headers["Cache-Control"] || "").includes("private, no-store") &&
      String(response.headers.Pragma || "").toLowerCase() === "no-cache");
  invalidBody.add(
    response.status !== 200 ||
      !privateDetail ||
      !validParticipationReviewResponse(operation, model, String(response.body || "").length),
  );
}

export function platformHealth() {
  const response = http.get(`${TARGET}/healthz`, { tags: { endpoint: "healthz" } });
  healthMs.add(response.timings.duration);
  healthFailure.add(response.status !== 200 || response.body !== "ok");
}
