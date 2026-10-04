import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
  durableEventFeedRequestUpperBound,
  durableFeedRequestUpperBound,
  durableSubmissionFeedRequestUpperBound,
  fallbackRequestUpperBound,
  steadyFallbackRequestsPerSecond,
} from "../realtime-recovery-model.js";

const recoverySource = readFileSync(
  new URL("../../../web/src/utils/SignalRRecovery.ts", import.meta.url),
  "utf8",
);
const hookSource = readFileSync(
  new URL("../../../web/src/hooks/useRecoveringHub.ts", import.meta.url),
  "utf8",
);
const eventSource = readFileSync(
  new URL("../../../web/src/pages/games/[id]/monitor/Events.tsx", import.meta.url),
  "utf8",
);

test("realtime HTTP fallbacks have a fixed-rate request-count ceiling", () => {
  assert.match(recoverySource, /NOTICE_FALLBACK_POLL_MS = 60_000/);
  assert.match(recoverySource, /OPERATOR_FALLBACK_POLL_MS = 30_000/);
  assert.match(recoverySource, /0\.9 \+ .*random\(\).* \* 0\.2/);
  assert.match(
    recoverySource,
    /if \(this\.refreshInFlight\) return this\.refreshInFlight/,
  );

  // 1,000 continuously visible clients held for two minutes. Counts include
  // the normal initial HTTP page read and one post-handshake reconciliation.
  assert.equal(
    fallbackRequestUpperBound({
      clients: 1_000,
      durationMs: 120_000,
      pollingIntervalMs: 60_000,
    }),
    4_000,
  );
  assert.equal(
    fallbackRequestUpperBound({
      clients: 1_000,
      durationMs: 120_000,
      pollingIntervalMs: 30_000,
    }),
    6_000,
  );
  assert.ok(steadyFallbackRequestsPerSecond(1_000, 60_000) < 19);
  assert.ok(steadyFallbackRequestsPerSecond(1_000, 30_000) < 38);
});

test("fallback ownership pauses hidden/offline clients and unmount cancels both timers", () => {
  assert.match(
    hookSource,
    /document\.visibilityState !== 'hidden' && navigator\.onLine/,
  );
  assert.match(
    hookSource,
    /document\.removeEventListener\('visibilitychange', resume\)/,
  );
  assert.match(hookSource, /window\.removeEventListener\('online', resume\)/);
  assert.match(hookSource, /stopPromise\.current = controller\.stop\(\)/);
  assert.match(hookSource, /return \{ state, waitForStop \}/);
  assert.throws(
    () =>
      fallbackRequestUpperBound({
        clients: 1,
        durationMs: 1,
        pollingIntervalMs: -1,
      }),
    /pollingIntervalMs/,
  );
});

test("durable event and submission recovery have explicit fixed request-count ceilings", () => {
  assert.match(eventSource, /const MAX_BACKFILL_PAGES = 10/);
  assert.match(eventSource, /page < MAX_BACKFILL_PAGES/);
  assert.match(eventSource, /const MAX_BUFFERED_EVENTS = 500/);
  assert.match(eventSource, /api\.game\.gameEventBackfill\(/);

  const workload = {
    clients: 1_000,
    durationMs: 120_000,
    pollingIntervalMs: 30_000,
    maxBackfillPages: 10,
  };
  assert.equal(durableFeedRequestUpperBound(workload), 61_000);
  assert.equal(durableEventFeedRequestUpperBound(workload), 61_000);
  assert.equal(durableSubmissionFeedRequestUpperBound(workload), 61_000);
  assert.equal(
    durableEventFeedRequestUpperBound(workload) + durableSubmissionFeedRequestUpperBound(workload),
    122_000,
  );

  for (const requestUpperBound of [durableEventFeedRequestUpperBound, durableSubmissionFeedRequestUpperBound]) {
    assert.throws(
      () => requestUpperBound({
        clients: 1,
        durationMs: 1,
        pollingIntervalMs: 30_000,
        maxBackfillPages: 0,
      }),
      /maxBackfillPages/,
    );
  }
});
