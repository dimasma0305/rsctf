import http from 'k6/http'
import { Rate, Trend } from 'k6/metrics'

import { DASHBOARD_OPERATIONS, validDashboardResponse } from '../admin-dashboard.js'

const TARGET = String(__ENV.TARGET || 'http://127.0.0.1:8080').replace(/\/+$/, '')
const ADMIN_TOKEN = __ENV.ADMIN_TOKEN || ''
const RATE = Number(__ENV.RATE || 1)
const VUS = Number(__ENV.VUS || Math.max(4, RATE * 2))

if (!ADMIN_TOKEN) throw new Error('ADMIN_TOKEN is required')
if (!Number.isSafeInteger(RATE) || RATE !== 1) {
  throw new Error('RATE must be 1 so one admin identity stays within the named query-work budget')
}
if (!Number.isSafeInteger(VUS) || VUS < 1) throw new Error('VUS must be a positive integer')

http.setResponseCallback(http.expectedStatuses(200))

const server5xx = new Rate('admin_dashboard_server_5xx')
const non200 = new Rate('admin_dashboard_non_200')
const invalidBody = new Rate('admin_dashboard_invalid_body')
const healthFailure = new Rate('admin_dashboard_health_failure')
const readMs = new Trend('admin_dashboard_read_ms', true)
const healthMs = new Trend('admin_dashboard_health_ms', true)

export const options = {
  scenarios: {
    dashboardReads: {
      executor: 'constant-arrival-rate',
      exec: 'dashboardReads',
      rate: RATE,
      timeUnit: '1s',
      duration: __ENV.DURATION || '30s',
      preAllocatedVUs: VUS,
      maxVUs: VUS * 2,
    },
    platformHealth: {
      executor: 'constant-arrival-rate',
      exec: 'platformHealth',
      rate: 1,
      timeUnit: '1s',
      duration: __ENV.DURATION || '30s',
      preAllocatedVUs: 2,
      maxVUs: 4,
    },
  },
  summaryTrendStats: ['avg', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    http_req_failed: ['rate==0'],
    admin_dashboard_server_5xx: ['rate==0'],
    admin_dashboard_non_200: ['rate==0'],
    admin_dashboard_invalid_body: ['rate==0'],
    admin_dashboard_health_failure: ['rate==0'],
    dropped_iterations: ['count==0'],
    admin_dashboard_read_ms: [`p(95)<${Number(__ENV.MAX_P95_MS || 1000)}`],
    admin_dashboard_health_ms: ['p(95)<500'],
  },
}

export function dashboardReads() {
  const operation = DASHBOARD_OPERATIONS[((__VU - 1) * 997 + __ITER) % DASHBOARD_OPERATIONS.length]
  const response = http.get(`${TARGET}${operation.path}`, {
    headers: { Authorization: `Bearer ${ADMIN_TOKEN}` },
    tags: { endpoint: operation.id },
  })
  readMs.add(response.timings.duration)
  server5xx.add(response.status >= 500)
  non200.add(response.status !== 200)
  let body
  try {
    body = response.json()
  } catch (_) {
    body = null
  }
  invalidBody.add(response.status !== 200 || !validDashboardResponse(operation, body))
}

export function platformHealth() {
  const response = http.get(`${TARGET}/healthz`, { tags: { endpoint: 'healthz' } })
  healthMs.add(response.timings.duration)
  healthFailure.add(response.status !== 200 || response.body !== 'ok')
}
