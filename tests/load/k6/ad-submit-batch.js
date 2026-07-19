// Fixed-rate A&D submit micro-harness. Every request contains the maximum
// 100-entry batch in one of three explicit shapes: one repeated known flag, 100
// distinct known flags, or 100 deterministic unknown flags. This isolates the
// lookup, eligibility, deduplication, and result-encoding paths without
// discovering credentials or flags from PostgreSQL.
import http from 'k6/http';
import { Counter, Trend } from 'k6/metrics';

import {
  AD_SUBMIT_STATUSES,
  distinctPlausibleFlags,
  inspectDistinctFlagBatch,
  inspectDistinctKnownFlagBatch,
  inspectKnownFlagBatch,
  MAX_AD_BATCH,
  parseAdSubmitBatchConfig,
} from '../ad-submit-batch.js';

const CONFIG = parseAdSubmitBatchConfig(__ENV);
const FLAGS =
  CONFIG.batchShape === 'distinct-known'
    ? CONFIG.explicitFlags
    : CONFIG.batchShape === 'distinct'
      ? distinctPlausibleFlags()
      : Array(MAX_AD_BATCH).fill(CONFIG.flag);
const BODY = JSON.stringify({ flags: FLAGS });

const submitMs = new Trend('ad_submit_ms', true);
const batches = new Counter('ad_submit_batches');
const results = new Counter('ad_submit_results');
const semanticInvalid = new Counter('ad_submit_semantic_invalid');
const server5xx = new Counter('server_5xx');
const rateLimited = new Counter('ad_submit_rate_limited');
const unexpectedHttpStatus = new Counter('ad_submit_unexpected_http_status');
const statusCounters = {};
for (const status of AD_SUBMIT_STATUSES) {
  statusCounters[status] = new Counter(`ad_submit_status_${status}`);
}

export const options = {
  scenarios: {
    ad_submit_batches: {
      executor: 'constant-arrival-rate',
      rate: CONFIG.rate,
      timeUnit: '1s',
      duration: CONFIG.duration,
      preAllocatedVUs: CONFIG.vus,
      maxVUs: CONFIG.maxVus,
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    dropped_iterations: ['count==0'],
    http_req_failed: ['rate==0'],
    server_5xx: ['count==0'],
    ad_submit_rate_limited: ['count==0'],
    ad_submit_unexpected_http_status: ['count==0'],
    ad_submit_semantic_invalid: ['count==0'],
  },
};

function recordStatusCounts(counts) {
  let total = 0;
  for (const status of AD_SUBMIT_STATUSES) {
    const count = counts[status] || 0;
    statusCounters[status].add(count);
    total += count;
  }
  results.add(total);
}

export default function () {
  const response = http.post(
    `${CONFIG.target}/api/Game/${CONFIG.gameId}/Ad/Submit`,
    BODY,
    {
      headers: {
        Authorization: `Bearer ${CONFIG.token}`,
        'Content-Type': 'application/json',
        'X-Real-IP': `32.1.${__VU % 250}.${(__VU * 17 + __ITER) % 250 + 1}`,
      },
      redirects: 0,
      timeout: CONFIG.requestTimeout,
      tags: { kind: `ad_submit_${CONFIG.batchShape}_batch` },
    },
  );

  batches.add(1);
  submitMs.add(response.timings.duration);
  server5xx.add(response.status >= 500 ? 1 : 0);
  rateLimited.add(response.status === 429 ? 1 : 0);
  unexpectedHttpStatus.add(response.status === 200 ? 0 : 1);
  if (response.status !== 200) {
    semanticInvalid.add(1);
    return;
  }

  let model;
  try {
    model = response.json();
  } catch {
    semanticInvalid.add(1);
    return;
  }
  const inspected =
    CONFIG.batchShape === 'distinct-known'
      ? inspectDistinctKnownFlagBatch(model, FLAGS)
      : CONFIG.batchShape === 'distinct'
        ? inspectDistinctFlagBatch(model, FLAGS)
        : inspectKnownFlagBatch(model, CONFIG.flag);
  recordStatusCounts(inspected.counts);
  semanticInvalid.add(inspected.valid ? 0 : 1);
}
