import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const source = (path: string) => readFileSync(path, 'utf8')

test('global notifications use the unobtrusive bottom-right anchor', () => {
  const app = source('src/App.tsx')
  assert.match(app, /<Notifications position="bottom-right"/)
  assert.doesNotMatch(app, /<Notifications position="top-right"/)
})

test('donations stay optional, server-backed, and bounded in the public UI', () => {
  const home = source('src/pages/Index.tsx')
  const panel = source('src/components/DonationPanel.tsx')
  const settings = source('src/pages/admin/Settings.tsx')
  const api = source('src/Api.ts')

  assert.match(home, /config\.donationsEnabled && <DonationPanel/)
  assert.match(panel, /useInfoGetDonations/)
  assert.doesNotMatch(panel, /supporterEmail|orderId|paymentMethod|apiKey/)
  assert.match(settings, /type="password"|<PasswordInput/)
  assert.match(settings, /leave blank to keep/)
  assert.match(api, /path: `\/api\/donations`/)
})
