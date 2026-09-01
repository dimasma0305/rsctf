import { lstatSync, readFileSync } from 'node:fs';

import { runK6, TARGET } from './lib.mjs';
import {
  endpointOriginMatchesTarget,
  validEndpointRows,
} from './proxy-traffic-admission.js';

const file = String(process.env.PROXY_TRAFFIC_ENDPOINTS_FILE || '');
if (!file) throw new Error('PROXY_TRAFFIC_ENDPOINTS_FILE is required');

let endpoints;
let fixtureContents = '';
try {
  const metadata = lstatSync(file);
  if (!metadata.isFile() || metadata.isSymbolicLink() || metadata.size > 6 * 1024 * 1024) {
    throw new Error('endpoint fixture must be a bounded regular file');
  }
  if ((metadata.mode & 0o077) !== 0) {
    throw new Error('endpoint fixture must not be accessible by group or other users');
  }
  fixtureContents = readFileSync(file, 'utf8');
  endpoints = JSON.parse(fixtureContents);
} catch {
  endpoints = null;
}
if (!validEndpointRows(endpoints)) {
  throw new Error('proxy traffic endpoints file is invalid');
}

const target = new URL(TARGET);
const origin = target.origin;
if (!endpoints.every((endpoint) => endpointOriginMatchesTarget(endpoint.url, TARGET))) {
  throw new Error('every proxy endpoint must use the TARGET origin');
}
if (process.env.PROXY_TRAFFIC_LOAD_ACK !== '1') {
  throw new Error('set PROXY_TRAFFIC_LOAD_ACK=1');
}
if (!['127.0.0.1', 'localhost', '::1'].includes(target.hostname) &&
    process.env.ALLOW_REMOTE_PROXY_TRAFFIC_LOAD !== origin) {
  throw new Error(`remote target requires ALLOW_REMOTE_PROXY_TRAFFIC_LOAD=${origin}`);
}

const status = runK6('proxy-traffic-admission.js', {
  TARGET,
  PROXY_TRAFFIC_ENDPOINTS: JSON.stringify(endpoints),
  RATE: process.env.RATE || 2,
  VUS: process.env.VUS || 20,
  MAX_VUS: process.env.MAX_VUS || 80,
  DURATION: process.env.DURATION || '30s',
  FRAME_BYTES: process.env.FRAME_BYTES || 65_536,
  FRAME_INTERVAL_MS: process.env.FRAME_INTERVAL_MS || 10,
  STREAM_MS: process.env.STREAM_MS || 10_000,
  SUMMARY_JSON: process.env.SUMMARY_JSON || '',
});
if (readFileSync(file, 'utf8') !== fixtureContents) {
  throw new Error('proxy traffic endpoint fixture changed during the run');
}
process.exit(status);
