// Fixed-rate authenticated smoke for the split player catalog/live projection.
import http from "k6/http";
import { Rate, Trend } from "k6/metrics";
import { validVisibleChallengeProjection } from "../details-read-model.js";

const TARGET = __ENV.TARGET || "http://127.0.0.1:8080";
const GAME = __ENV.GAME || "";
const TOKENS = JSON.parse(open(__ENV.TOKENS_FILE || ""));
const RATE = Number(__ENV.RATE || 10);
const VUS = Number(__ENV.VUS || Math.max(10, RATE));
const DURATION = __ENV.DURATION || "30s";
const REQUIRE_FIXED_PROJECTION = __ENV.REQUIRE_FIXED_PROJECTION !== "0";
const durationMatch = /^(\d+)(s|m)$/.exec(DURATION);
const durationSeconds = durationMatch
  ? Number(durationMatch[1]) * (durationMatch[2] === "m" ? 60 : 1)
  : Number.NaN;

if (!/^\d+$/.test(GAME) || TOKENS.length === 0 || TOKENS.length > 4000) {
  throw new Error(
    "GAME and between 1 and 4000 accepted-participant tokens are required",
  );
}
if (!Number.isSafeInteger(RATE) || RATE <= 0 || RATE > 2000) {
  throw new Error("RATE must be an integer between 1 and 2000");
}
if (!Number.isSafeInteger(VUS) || VUS <= 0 || VUS > 500) {
  throw new Error("VUS must be an integer between 1 and 500");
}
if (!Number.isFinite(durationSeconds) || durationSeconds <= 0 || durationSeconds > 600) {
  throw new Error("DURATION must be between 1s and 10m");
}

const non200 = new Rate("details_non_200");
const server5xx = new Rate("details_server_5xx");
const invalidJson = new Rate("details_invalid_json");
const invalidProjection = new Rate("details_invalid_visible_projection");
const duration = new Trend("details_read_ms", true);

const thresholds = {
  details_non_200: ["rate==0"],
  details_server_5xx: ["rate==0"],
  details_invalid_json: ["rate==0"],
  dropped_iterations: ["count==0"],
  details_read_ms: ["p(95)<800"],
};
if (REQUIRE_FIXED_PROJECTION)
  thresholds.details_invalid_visible_projection = ["rate==0"];

export const options = {
  scenarios: {
    challengeDetails: {
      executor: "constant-arrival-rate",
      rate: RATE,
      timeUnit: "1s",
      duration: DURATION,
      preAllocatedVUs: VUS,
      maxVUs: Math.min(500, VUS * 2),
    },
  },
  summaryTrendStats: ["avg", "med", "p(90)", "p(95)", "p(99)", "max"],
  thresholds,
};

function sourceIp(index) {
  return `31.${1 + (index % 240)}.${1 + (Math.floor(index / 240) % 250)}.${1 + (index % 250)}`;
}

let catalogModel = null;
let participantModel = null;
let participantEtag = "";

export default function () {
  const tokenIndex = ((__VU - 1) * 997 + __ITER) % TOKENS.length;
  const headers = {
    Authorization: `Bearer ${TOKENS[tokenIndex]}`,
    "X-Real-IP": sourceIp(tokenIndex),
  };
  if (catalogModel === null) {
    const catalog = http.get(`${TARGET}/api/game/${GAME}/details/catalog`, {
      headers,
      tags: { endpoint: "challenge_catalog" },
    });
    if (catalog.status === 200) {
      try {
        catalogModel = catalog.json();
      } catch (_) {
        catalogModel = null;
      }
    }
    non200.add(catalog.status !== 200);
    server5xx.add(catalog.status >= 500);
  }

  const response = http.get(`${TARGET}/api/game/${GAME}/details/live`, {
    headers: {
      ...headers,
      ...(participantEtag ? { "If-None-Match": participantEtag } : {}),
    },
    tags: { endpoint: "participant_delta" },
  });

  duration.add(response.timings.duration);
  non200.add(response.status !== 200 && response.status !== 304);
  server5xx.add(response.status >= 500);

  if (response.status === 200) {
    try {
      participantModel = response.json();
      participantEtag = response.headers.ETag || response.headers.Etag || "";
    } catch (_) {
      participantModel = null;
    }
  }
  const model =
    catalogModel && participantModel
      ? { ...catalogModel, rank: participantModel.rank }
      : null;
  invalidJson.add(
    (response.status === 200 || response.status === 304) && model === null,
  );
  invalidProjection.add(
    (response.status !== 200 && response.status !== 304) ||
      !validVisibleChallengeProjection(model),
  );
}
