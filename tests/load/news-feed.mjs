// Fixed-rate, read-only acceptance for the bounded homepage news feed.
import { runK6, TARGET } from './lib.mjs';

if (process.env.NEWS_FEED_STRESS_ACK !== '1') {
  throw new Error('NEWS_FEED_STRESS_ACK=1 is required before loading the public homepage feed');
}

const rate = Number(process.env.RATE || 2);
const vus = Number(process.env.VUS || 20);
const duration = String(process.env.DURATION || '30s');
if (!Number.isSafeInteger(rate) || rate < 1 || rate > 500) {
  throw new Error('RATE must be an integer between 1 and 500 requests/second');
}
if (!Number.isSafeInteger(vus) || vus < 1 || vus > 500) {
  throw new Error('VUS must be an integer between 1 and 500');
}
if (!/^([1-9][0-9]*)(s|m)$/.test(duration)) {
  throw new Error('DURATION must use positive k6 seconds or minutes');
}

const exactHealth = async () => {
  const response = await fetch(new URL('/healthz', TARGET));
  return response.status === 200 && (await response.text()) === 'ok';
};

if (!(await exactHealth())) throw new Error('target healthz is not exactly HTTP 200 / ok');

console.log(`homepage news feed load → ${TARGET} rate=${rate}/s duration=${duration}`);
const status = runK6('news-feed.js', {
  TARGET,
  RATE: rate,
  VUS: vus,
  DURATION: duration,
  SUMMARY_JSON: process.env.SUMMARY_JSON || '',
});
if (status !== 0) process.exit(status);

if (!(await exactHealth())) throw new Error('target healthz failed after the news-feed load');
