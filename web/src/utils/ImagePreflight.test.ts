import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import type { ControlJobModel, ImagePreflightModel } from '@Api'
import {
  PREFLIGHT_POLL_MS,
  PREFLIGHT_STATE_COLOR,
  formatPreflightBytes,
  formatPreflightCpu,
  formatPreflightDuration,
  isPreflightActive,
  preflightPollDelay,
  preflightRowState,
} from './ImagePreflight'

const job = (status: ControlJobModel['status']): ControlJobModel => ({
  id: 'job',
  kind: 'ImagePreflight',
  scopeKey: 'game:1:image-preflight',
  gameId: 1,
  operationId: 'op',
  fingerprint: 'f',
  status,
  progressCurrent: 0,
  progressTotal: 1,
  requestedGeneration: 1,
  cancellationRequested: false,
  createdAtUtc: 1,
  updatedAtUtc: 1,
})

test('preflight rows map pull and start steps to one labelled state', () => {
  assert.equal(preflightRowState({ pullStatus: 'Pending', startStatus: 'Pending' }), 'pending')
  assert.equal(preflightRowState({ pullStatus: 'Running', startStatus: 'Pending' }), 'running')
  assert.equal(preflightRowState({ pullStatus: 'Succeeded', startStatus: 'Pending' }), 'pending')
  assert.equal(preflightRowState({ pullStatus: 'Succeeded', startStatus: 'Succeeded' }), 'succeeded')
  assert.equal(preflightRowState({ pullStatus: 'Skipped', startStatus: 'Succeeded' }), 'succeeded')
  assert.equal(preflightRowState({ pullStatus: 'Failed', startStatus: 'Skipped' }), 'failed')
  assert.equal(preflightRowState({ pullStatus: 'Succeeded', startStatus: 'Failed' }), 'failed')
  assert.equal(preflightRowState({ pullStatus: 'Skipped', startStatus: 'Skipped' }), 'skipped')
  for (const state of ['pending', 'running', 'succeeded', 'failed', 'skipped'] as const) {
    assert.ok(PREFLIGHT_STATE_COLOR[state])
  }
})

test('preflight polling stops on every terminal job state and when no job exists', () => {
  assert.equal(preflightPollDelay(undefined), null)
  assert.equal(preflightPollDelay({ results: [] }), null)
  assert.equal(preflightPollDelay({ job: null, results: [] }), null)
  for (const status of ['Queued', 'Running'] as const) {
    const data: ImagePreflightModel = { job: job(status), results: [] }
    assert.equal(preflightPollDelay(data), PREFLIGHT_POLL_MS)
    assert.equal(isPreflightActive(data.job), true)
  }
  for (const status of ['Succeeded', 'Failed', 'Cancelled'] as const) {
    assert.equal(preflightPollDelay({ job: job(status), results: [] }), null)
    assert.equal(isPreflightActive(job(status)), false)
  }
  assert.ok(PREFLIGHT_POLL_MS >= 1000)
})

test('preflight capacity and duration formatting is bounded and readable', () => {
  assert.equal(formatPreflightCpu(0), '0 vCPU')
  assert.equal(formatPreflightCpu(2500), '2.5 vCPU')
  assert.equal(formatPreflightCpu(4000), '4 vCPU')
  assert.equal(formatPreflightBytes(NaN), '0 MiB')
  assert.equal(formatPreflightBytes(64 * 1024 ** 2), '64 MiB')
  assert.equal(formatPreflightBytes(1.5 * 1024 ** 3), '1.5 GiB')
  assert.equal(formatPreflightBytes(12 * 1024 ** 3), '12 GiB')
  assert.equal(formatPreflightDuration(0), '—')
  assert.equal(formatPreflightDuration(850), '850ms')
  assert.equal(formatPreflightDuration(1500), '1.5s')
  assert.equal(formatPreflightDuration(65_000), '1m 5s')
})

test('the preflight panel uses the bounded poller, single-flight start, and translated labels', () => {
  const panel = readFileSync('src/components/admin/ImagePreflightPanel.tsx', 'utf8')
  assert.match(panel, /useCompletionPolling/)
  assert.match(panel, /successDelay: preflightPollDelay/)
  assert.match(panel, /CompletionPollSWRConfig/)
  assert.match(panel, /preflightFlight/)
  assert.match(panel, /createOperationId\(\)/)
  assert.match(panel, /startControlJob/)
  assert.match(panel, /startImagePreflight\(\s*eventId,\s*operationId/)
  assert.doesNotMatch(panel, /setInterval|setTimeout|refreshInterval/)
  assert.match(panel, /disabled=\{[^}]*active/)
  const en = JSON.parse(readFileSync('src/locales/en-US/admin.json', 'utf8')).readiness.preflight
  for (const state of ['pending', 'running', 'succeeded', 'failed', 'skipped']) assert.ok(en.states[state])
  for (const status of ['Queued', 'Running', 'Succeeded', 'Failed', 'Cancelled']) assert.ok(en.job[status])
  const api = readFileSync('src/Api.ts', 'utf8')
  assert.match(api, /startImagePreflight:[\s\S]*"Idempotency-Key": operationId/)
})
