import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
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
