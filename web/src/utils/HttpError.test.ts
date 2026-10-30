import assert from 'node:assert/strict'
import test from 'node:test'
import { httpErrorHelp } from './HttpError'

test('request failures give a useful next step without exposing request credentials', () => {
  assert.match(httpErrorHelp({ response: { status: 401 }, request: { password: 'secret' } })!.fallback, /Sign in/)
  assert.match(httpErrorHelp({ status: 409 })!.fallback, /Reload/)
  assert.match(httpErrorHelp({ status: 429 })!.fallback, /Wait/)
  assert.match(httpErrorHelp({ status: 503 })!.fallback, /server/)
  assert.match(httpErrorHelp({ request: {} })!.fallback, /connection/)
  assert.equal(httpErrorHelp(null), null)
  assert.equal(httpErrorHelp({ status: 400 }), null)
})
