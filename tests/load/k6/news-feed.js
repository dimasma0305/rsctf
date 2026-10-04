// Fixed-rate read-only load for the bounded, conditionally cached homepage feed.
import http from 'k6/http';
import { Rate, Trend } from 'k6/metrics';

const TARGET = __ENV.TARGET || 'http://127.0.0.1:8080';
const RATE = Number(__ENV.RATE || 2);
const VUS = Number(__ENV.VUS || 20);

if (!Number.isSafeInteger(RATE) || RATE < 1 || !Number.isSafeInteger(VUS) || VUS < 1) {
  throw new Error('RATE and VUS must be positive integers');
}

const invalidFeed = new Rate('invalid_feed');
const server5xx = new Rate('server_5xx');
const conditionalMiss = new Rate('conditional_miss');
const homepageFeedMs = new Trend('homepage_feed_ms', true);

export const options = {
  scenarios: {
    homepageFeed: {
      executor: 'constant-arrival-rate',
      rate: RATE,
      timeUnit: '1s',
      duration: __ENV.DURATION || '30s',
      preAllocatedVUs: VUS,
      maxVUs: VUS * 4,
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    invalid_feed: ['rate==0'],
    conditional_miss: ['rate==0'],
    server_5xx: ['rate==0'],
    dropped_iterations: ['count==0'],
    http_req_duration: ['p(95)<1000'],
  },
};

const responseEtag = (response) =>
  response.headers.ETag || response.headers.Etag || response.headers.etag || '';

export function setup() {
  const response = http.get(`${TARGET}/api/posts/latest`, {
    responseType: 'text',
    tags: { endpoint: 'homepage_feed_seed' },
  });
  let posts = null;
  try {
    posts = response.json();
  } catch (_) {
    // The explicit setup error below reports a malformed response.
  }
  const etag = responseEtag(response);
  if (response.status !== 200 || !Array.isArray(posts) || posts.length > 20 || !etag) {
    throw new Error('homepage feed seed must be HTTP 200, bounded to 20 rows, and expose an ETag');
  }
  return { etag };
}

export default function ({ etag }) {
  const response = http.get(`${TARGET}/api/posts/latest`, {
    headers: { 'If-None-Match': etag },
    responseType: 'text',
    tags: { endpoint: 'homepage_feed_conditional' },
  });
  homepageFeedMs.add(response.timings.duration);
  server5xx.add(response.status >= 500);
  conditionalMiss.add(response.status !== 304);
  invalidFeed.add(
    response.status !== 304 ||
      String(response.body || '').length !== 0 ||
      responseEtag(response) !== etag,
  );
}
