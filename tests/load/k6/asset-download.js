// One authenticated, deterministic byte range per iteration. This measures
// delivery at a held request and byte rate; it is not a peak-throughput test.
import http from "k6/http";
import { check } from "k6";
import { Rate, Trend } from "k6/metrics";

import { assetHashFromPath, assetRange, rotatingAssetHash } from "../asset-download-model.js";

const TARGET = __ENV.TARGET || "http://127.0.0.1:8080";
const ASSET_URL = __ENV.ASSET_URL || "";
const ASSET_SIZE = Number(__ENV.ASSET_SIZE);
const RANGE_BYTES = Number(__ENV.RANGE_BYTES || 1048576);
const TOKENS = JSON.parse(open(__ENV.TOKENS_FILE || ""));
const RATE = Number(__ENV.RATE || 20);
const VUS = Number(__ENV.VUS || 64);
const UNKNOWN_RATE = Number(__ENV.UNKNOWN_RATE || 32);
const CONDITIONAL_RATE = Number(__ENV.CONDITIONAL_RATE || 20);
const PUBLIC_ASSET_URL = __ENV.PUBLIC_ASSET_URL || "";
const PUBLIC_HASH = assetHashFromPath(PUBLIC_ASSET_URL);

if (
  !ASSET_URL.startsWith("/assets/") ||
  !Number.isSafeInteger(ASSET_SIZE) ||
  ASSET_SIZE <= 0 ||
  !Number.isSafeInteger(RANGE_BYTES) ||
  RANGE_BYTES <= 0 ||
  RANGE_BYTES > ASSET_SIZE ||
  !Number.isSafeInteger(RATE) ||
  RATE <= 0 ||
  !Number.isSafeInteger(VUS) ||
  VUS <= 0 ||
  !Number.isSafeInteger(UNKNOWN_RATE) ||
  UNKNOWN_RATE <= 0 ||
  !Number.isSafeInteger(CONDITIONAL_RATE) ||
  CONDITIONAL_RATE <= 0 ||
  !PUBLIC_HASH ||
  TOKENS.length === 0
) {
  throw new Error(
    "valid ASSET_URL, PUBLIC_ASSET_URL, sizes, fixed rates, VUS, and TOKENS_FILE are required",
  );
}

const rangeMs = new Trend("asset_range_ms", true);
const server5xx = new Rate("server_5xx");
const authRejected = new Rate("auth_rejected");
const rangeInvalid = new Rate("range_invalid");
const unknownInvalid = new Rate("unknown_invalid");
const conditionalInvalid = new Rate("conditional_invalid");
const healthInvalid = new Rate("health_invalid");

export const options = {
  discardResponseBodies: true,
  scenarios: {
    attachmentRanges: {
      executor: "constant-arrival-rate",
      rate: RATE,
      timeUnit: "1s",
      duration: __ENV.DURATION || "30s",
      preAllocatedVUs: VUS,
      maxVUs: VUS * 4,
      exec: "attachmentRange",
    },
    unknownHashRotation: {
      executor: "constant-arrival-rate",
      rate: UNKNOWN_RATE,
      timeUnit: "1s",
      duration: __ENV.DURATION || "30s",
      preAllocatedVUs: Math.max(8, Math.ceil(VUS / 2)),
      maxVUs: VUS * 2,
      exec: "unknownHash",
    },
    publicConditional: {
      executor: "constant-arrival-rate",
      rate: CONDITIONAL_RATE,
      timeUnit: "1s",
      duration: __ENV.DURATION || "30s",
      preAllocatedVUs: Math.max(8, Math.ceil(VUS / 2)),
      maxVUs: VUS * 2,
      exec: "publicConditional",
    },
    health: {
      executor: "constant-arrival-rate",
      rate: 2,
      timeUnit: "1s",
      duration: __ENV.DURATION || "30s",
      preAllocatedVUs: 2,
      maxVUs: 8,
      exec: "health",
    },
  },
  summaryTrendStats: ["avg", "med", "p(90)", "p(95)", "p(99)", "max"],
  thresholds: {
    server_5xx: ["rate==0"],
    auth_rejected: ["rate==0"],
    range_invalid: ["rate==0"],
    unknown_invalid: ["rate==0"],
    conditional_invalid: ["rate==0"],
    health_invalid: ["rate==0"],
    dropped_iterations: ["count==0"],
    asset_range_ms: ["p(95)<2000"],
  },
};

function expectedOverload(response) {
  return (response.status === 429 || response.status === 503) && Boolean(response.headers["Retry-After"]);
}

function recordServerFailure(response) {
  server5xx.add(response.status >= 500 && !expectedOverload(response));
}

export function attachmentRange() {
  const sequence = (__VU - 1) * 997 + __ITER;
  const {
    start,
    end,
    length: expectedLength,
  } = assetRange(sequence, ASSET_SIZE, RANGE_BYTES);
  const response = http.get(`${TARGET}${ASSET_URL}`, {
    headers: {
      Authorization: `Bearer ${TOKENS[sequence % TOKENS.length]}`,
      Range: `bytes=${start}-${end}`,
    },
    redirects: 2,
    responseType: "none",
    tags: { endpoint: "asset_range" },
  });

  const contentRange = String(response.headers["Content-Range"] || "");
  const contentLength = Number(response.headers["Content-Length"]);
  const valid =
    response.status === 206 &&
    contentRange === `bytes ${start}-${end}/${ASSET_SIZE}` &&
    contentLength === expectedLength &&
    String(response.headers["Accept-Ranges"] || "").toLowerCase() === "bytes";

  rangeMs.add(response.timings.duration);
  recordServerFailure(response);
  authRejected.add(response.status === 401 || response.status === 403);
  rangeInvalid.add(!valid);
  check(response, { "attachment range is exact": () => valid });
}

export function unknownHash() {
  const sequence = (__VU - 1) * 104729 + __ITER;
  const hash = rotatingAssetHash(sequence);
  const response = http.get(`${TARGET}/assets/${hash}/unknown.bin`, {
    redirects: 0,
    responseType: "none",
    tags: { endpoint: "asset_unknown_hash" },
  });
  const valid = response.status === 403 || expectedOverload(response);
  recordServerFailure(response);
  unknownInvalid.add(!valid);
  check(response, { "unknown hash is denied or retryably admitted": () => valid });
}

export function publicConditional() {
  const etag = `"${PUBLIC_HASH.slice(8, 16)}"`;
  const response = http.get(`${TARGET}${PUBLIC_ASSET_URL}`, {
    headers: { "If-None-Match": etag },
    redirects: 0,
    responseType: "none",
    tags: { endpoint: "asset_public_304" },
  });
  const valid = response.status === 304 || expectedOverload(response);
  recordServerFailure(response);
  conditionalInvalid.add(!valid);
  check(response, { "public immutable asset revalidates without a body": () => valid });
}

export function health() {
  const response = http.get(`${TARGET}/healthz`, {
    responseType: "none",
    tags: { endpoint: "asset_load_health" },
  });
  const valid = response.status === 200;
  server5xx.add(response.status >= 500);
  healthInvalid.add(!valid);
  check(response, { "health remains responsive": () => valid });
}
