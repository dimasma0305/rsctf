import assert from "node:assert/strict";
import test from "node:test";

import {
  EVENT_TELEMETRY_LOGICAL_LIMIT,
  boundedInteger,
  k6PhaseSummary,
  parsePeerFixture,
  parseProcessStat,
  parseUsage,
  summarizeResourceSamples,
} from "../event-security-load.js";

test("event telemetry stress inputs and the 256 MiB bound are exact", () => {
  assert.equal(EVENT_TELEMETRY_LOGICAL_LIMIT, 268435456);
  assert.equal(boundedInteger("4096", "rows", 1, 4096), 4096);
  assert.throws(() => boundedInteger("4097", "rows", 1, 4096));
  assert.deepEqual(
    parsePeerFixture("00000000-0000-4000-8000-000000000001|7|00000000-0000-4000-8000-000000000002|1767225600000"),
    {
      userId: "00000000-0000-4000-8000-000000000001",
      participationId: 7,
      peerId: "00000000-0000-4000-8000-000000000002",
      bucketMs: 1767225600000,
    },
  );
  assert.throws(() => parsePeerFixture("bad|7|bad|1"));
  assert.deepEqual(parseUsage("192|1|f|65536"), {
    logicalBytes: 192,
    rowCount: 1,
    disabled: false,
    physicalBytes: 65536,
  });
});

test("resource and k6 summaries preserve fixed-rate comparison fields", () => {
  const first = parseProcessStat("123 (rsctf worker) S 0 0 0 0 0 0 0 0 0 0 10 5 0 0 0 0 0 0 0 0 2048", 100, 4096, null, 1_000);
  assert.equal(first.sample, null);
  const second = parseProcessStat(
    "123 (rsctf worker) S 0 0 0 0 0 0 0 0 0 0 20 15 0 0 0 0 0 0 0 0 3072",
    100,
    4096,
    first.state,
    2_000,
  );
  assert.deepEqual(second.sample, {
    name: "rsctf-process",
    cpuPercent: 20,
    memoryBytes: 12 * 1024 * 1024,
  });
  assert.throws(() => parseProcessStat("not-a-sample", 100, 4096));
  assert.deepEqual(
    summarizeResourceSamples([
      { containers: [{ name: "app", cpuPercent: 10, memoryBytes: 100 }] },
      { containers: [{ name: "app", cpuPercent: 30, memoryBytes: 120 }] },
    ]),
    [{ name: "app", samples: 2, averageCpuPercent: 20, maxCpuPercent: 30, maxMemoryBytes: 120 }],
  );
  const result = k6PhaseSummary({
    metrics: {
      http_reqs: { values: { count: 10, rate: 2 } },
      event_security_ingest_ms: { values: { med: 4, "p(95)": 8, "p(99)": 9, max: 11 } },
      server_5xx: { values: { rate: 0 } },
      invalid_response: { values: { rate: 0 } },
      quota_dropped: { values: { rate: 0.1 } },
      dropped_iterations: { values: { count: 0 } },
    },
  });
  assert.equal(result.p95Ms, 8);
  assert.equal(result.quotaDropRate, 0.1);

  const k6v2 = k6PhaseSummary({
    metrics: {
      http_reqs: { count: 10, rate: 2 },
      event_security_ingest_ms: { med: 4, "p(95)": 8, "p(99)": 9, max: 11 },
      server_5xx: { value: 0, passes: 0, fails: 10 },
      invalid_response: { value: 0, passes: 0, fails: 10 },
      quota_dropped: { value: 0.1, passes: 1, fails: 9 },
      dropped_iterations: { count: 0, rate: 0 },
    },
  });
  assert.deepEqual(k6v2, result);
});
