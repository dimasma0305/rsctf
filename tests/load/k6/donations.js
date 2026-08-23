// Fixed-rate read-only load for the bounded, cached public donation feed.
import http from 'k6/http';
import { Rate, Trend } from 'k6/metrics';

const TARGET = __ENV.TARGET || 'http://127.0.0.1:8080';
const RATE = Number(__ENV.RATE || 50);
const VUS = Number(__ENV.VUS || 20);

if (!Number.isSafeInteger(RATE) || RATE < 1 || !Number.isSafeInteger(VUS) || VUS < 1) {
  throw new Error('RATE and VUS must be positive integers');
}

const invalidFeed = new Rate('invalid_feed');
const privacyLeak = new Rate('privacy_leak');
const server5xx = new Rate('server_5xx');
const donationFeedMs = new Trend('donation_feed_ms', true);

export const options = {
  scenarios: {
    donationFeed: {
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
    privacy_leak: ['rate==0'],
    server_5xx: ['rate==0'],
    dropped_iterations: ['count==0'],
    http_req_duration: ['p(95)<1000'],
  },
};

export default function () {
  const response = http.get(`${TARGET}/api/donations`, {
    responseType: 'text',
    tags: { endpoint: 'donation_feed' },
  });
  donationFeedMs.add(response.timings.duration);
  server5xx.add(response.status >= 500);

  let feed = null;
  try {
    feed = response.json();
  } catch (_) {
    // Reported through invalid_feed below.
  }
  invalidFeed.add(
    response.status !== 200 ||
      !feed ||
      feed.provider !== 'Trakteer' ||
      feed.currency !== 'IDR' ||
      !Array.isArray(feed.leaderboard) ||
      feed.leaderboard.length > 10 ||
      !Array.isArray(feed.messages) ||
      feed.messages.length > 20,
  );

  const body = String(response.body || '').toLowerCase();
  privacyLeak.add(
    body.includes('apikey') ||
      body.includes('supporteremail') ||
      body.includes('orderid') ||
      body.includes('paymentmethod'),
  );
}
