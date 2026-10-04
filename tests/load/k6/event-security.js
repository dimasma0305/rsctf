// Fixed-rate aggregate telemetry ingestion. MODE=control sends an empty batch;
// MODE=ingest sends unique five-minute aggregate rows for the same live peer.
import http from "k6/http";
import { check } from "k6";
import { Rate, Trend } from "k6/metrics";

const TARGET = __ENV.TARGET || "http://127.0.0.1:8080";
const TOKEN = __ENV.EVENT_SENSOR_TOKEN || "";
const MODE = __ENV.MODE || "control";
const fixture = JSON.parse(open(__ENV.EVENT_SECURITY_FIXTURE || ""));
const RATE = Number(__ENV.RATE || 2);
const VUS = Number(__ENV.VUS || 16);
const ROWS = Number(__ENV.ROWS_PER_BATCH || 256);

if (
  !["control", "ingest"].includes(MODE) || TOKEN.length < 32 ||
  !Number.isSafeInteger(RATE) || RATE < 1 ||
  !Number.isSafeInteger(VUS) || VUS < 1 ||
  !Number.isSafeInteger(ROWS) || ROWS < 1 || ROWS > 4096 ||
  !Number.isSafeInteger(fixture.gameId) || fixture.gameId <= 0
) {
  throw new Error("valid event-security mode, token, fixture, RATE, VUS, and ROWS_PER_BATCH are required");
}

const ingestMs = new Trend("event_security_ingest_ms", true);
const server5xx = new Rate("server_5xx");
const invalidResponse = new Rate("invalid_response");
const quotaDropped = new Rate("quota_dropped");

export const options = {
  scenarios: {
    telemetry: {
      executor: "constant-arrival-rate",
      rate: RATE,
      timeUnit: "1s",
      duration: __ENV.DURATION || "60s",
      preAllocatedVUs: VUS,
      maxVUs: VUS * 4,
    },
  },
  summaryTrendStats: ["avg", "med", "p(90)", "p(95)", "p(99)", "max"],
  thresholds: {
    server_5xx: ["rate==0"],
    invalid_response: ["rate==0"],
    dropped_iterations: ["count==0"],
    event_security_ingest_ms: ["p(95)<2000"],
  },
};

function rowsForIteration() {
  if (MODE === "control") return [];
  const sequence = ((__VU - 1) * 10_000_000 + __ITER * ROWS) % 2_000_000_000;
  return Array.from({ length: ROWS }, (_, index) => ({
    userId: fixture.userId,
    participationId: fixture.participationId,
    peerId: fixture.peerId,
    challengeId: null,
    containerGeneration: sequence + index,
    bucketStartUtc: fixture.bucketMs,
    packetsUp: 4,
    packetsDown: 4,
    bytesUp: 1024,
    bytesDown: 2048,
    distinctDestinations: 2,
    connectionCount: 1,
    activeSeconds: 30,
  }));
}

// A semantic k6 iteration owns one stable UUID-shaped batch identity. k6 may
// repeat an HTTP request at the transport layer, and the server must replay
// that exact result without charging quota or drop counters twice.
function batchIdForIteration() {
  const vu = (__VU >>> 0).toString(16).padStart(8, "0");
  const iteration = (__ITER >>> 0).toString(16).padStart(12, "0");
  return `${vu}-0000-4000-8000-${iteration}`;
}

export default function () {
  const response = http.post(
    `${TARGET}/api/internal/event-security/telemetry`,
    JSON.stringify({
      batchId: batchIdForIteration(),
      gameId: fixture.gameId,
      flows: rowsForIteration(),
      dnsProviders: [],
      peerNetworks: [],
      flagTransports: [],
      sensorDroppedRows: 0,
      sensorDroppedBytes: 0,
    }),
    {
      headers: {
        Authorization: `Bearer ${TOKEN}`,
        "Content-Type": "application/json",
      },
      tags: { endpoint: `event_security_${MODE}` },
    },
  );
  let result;
  try {
    const body = response.json();
    result = body?.data?.data ?? body?.data ?? body;
  } catch {
    result = null;
  }
  const valid = response.status === 200 && Number.isSafeInteger(result?.acceptedRows);
  ingestMs.add(response.timings.duration);
  server5xx.add(response.status >= 500);
  invalidResponse.add(!valid);
  quotaDropped.add(Boolean(result?.droppedForQuota));
  check(response, { "bounded telemetry response is valid": () => valid });
}
