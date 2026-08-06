// One authenticated, deterministic byte range per iteration. This measures
// delivery at a held request and byte rate; it is not a peak-throughput test.
import http from "k6/http";
import { check } from "k6";
import { Rate, Trend } from "k6/metrics";

import { assetRange } from "../asset-download-model.js";

const TARGET = __ENV.TARGET || "http://127.0.0.1:8080";
const ASSET_URL = __ENV.ASSET_URL || "";
const ASSET_SIZE = Number(__ENV.ASSET_SIZE);
const RANGE_BYTES = Number(__ENV.RANGE_BYTES || 1048576);
const TOKENS = JSON.parse(open(__ENV.TOKENS_FILE || ""));
const RATE = Number(__ENV.RATE || 20);
const VUS = Number(__ENV.VUS || 64);

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
  TOKENS.length === 0
) {
  throw new Error(
    "valid ASSET_URL, ASSET_SIZE, RANGE_BYTES, RATE, VUS, and TOKENS_FILE are required",
  );
}

const rangeMs = new Trend("asset_range_ms", true);
const server5xx = new Rate("server_5xx");
const authRejected = new Rate("auth_rejected");
const rangeInvalid = new Rate("range_invalid");

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
    },
  },
  summaryTrendStats: ["avg", "med", "p(90)", "p(95)", "p(99)", "max"],
  thresholds: {
    server_5xx: ["rate==0"],
    auth_rejected: ["rate==0"],
    range_invalid: ["rate==0"],
    dropped_iterations: ["count==0"],
    asset_range_ms: ["p(95)<2000"],
  },
};

export default function () {
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
  server5xx.add(response.status >= 500);
  authRejected.add(response.status === 401 || response.status === 403);
  rangeInvalid.add(!valid);
  check(response, { "attachment range is exact": () => valid });
}
