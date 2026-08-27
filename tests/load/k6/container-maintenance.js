// Fixed-rate public reads while the disposable stack drains a container backlog.
import http from "k6/http";
import { Rate, Trend } from "k6/metrics";

const TARGET = __ENV.TARGET || "http://127.0.0.1:8080";
const RATE = Number(__ENV.RATE || 20);
const VUS = Number(__ENV.VUS || Math.max(20, RATE));

if (!Number.isSafeInteger(RATE) || RATE <= 0 || !Number.isSafeInteger(VUS) || VUS <= 0) {
  throw new Error("RATE and VUS must be positive integers");
}

const non200 = new Rate("maintenance_non_200");
const server5xx = new Rate("maintenance_server_5xx");
const invalidBody = new Rate("maintenance_invalid_body");
const latency = new Trend("maintenance_request_ms", true);

export const options = {
  scenarios: {
    publicReads: {
      executor: "constant-arrival-rate",
      rate: RATE,
      timeUnit: "1s",
      duration: __ENV.DURATION || "70s",
      preAllocatedVUs: VUS,
      maxVUs: VUS * 2,
    },
  },
  summaryTrendStats: ["avg", "med", "p(90)", "p(95)", "p(99)", "max"],
  thresholds: {
    maintenance_non_200: ["rate==0"],
    maintenance_server_5xx: ["rate==0"],
    maintenance_invalid_body: ["rate==0"],
    dropped_iterations: ["count==0"],
    maintenance_request_ms: ["p(95)<800"],
  },
};

export default function () {
  const health = __ITER % 2 === 0;
  const response = http.get(`${TARGET}${health ? "/healthz" : "/api/game"}`, {
    tags: { endpoint: health ? "healthz" : "games" },
  });
  latency.add(response.timings.duration);
  non200.add(response.status !== 200);
  server5xx.add(response.status >= 500);
  let valid = response.status === 200;
  if (valid && health) valid = response.body === "ok";
  if (valid && !health) {
    try {
      valid = Array.isArray(response.json());
    } catch (_) {
      valid = false;
    }
  }
  invalidBody.add(!valid);
}
