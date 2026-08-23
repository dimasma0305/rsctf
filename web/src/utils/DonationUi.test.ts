import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const source = (path: string) => readFileSync(path, 'utf8')

test('global notifications use the unobtrusive bottom-right anchor', () => {
  const app = source('src/App.tsx')
  assert.match(app, /<Notifications position="bottom-right"/)
  assert.doesNotMatch(app, /<Notifications position="top-right"/)
})

test('donations use a feature-gated page instead of the home feed', () => {
  const home = source('src/pages/Index.tsx')
  const donations = source('src/pages/Donations.tsx')
  const navigation = source('src/components/navigation.ts')
  const navbar = source('src/components/AppNavbar.tsx')
  const mobileHeader = source('src/components/AppHeader.tsx')
  const panel = source('src/components/DonationPanel.tsx')
  const settings = source('src/pages/admin/Settings.tsx')
  const api = source('src/Api.ts')

  assert.doesNotMatch(home, /DonationPanel|donationsEnabled/)
  assert.match(donations, /<DonationPanel/)
  assert.match(donations, /if \(loading\) return <WithNavBar isLoading/)
  assert.match(donations, /!config\.donationsEnabled/)
  assert.match(navigation, /link: '\/donations'/)
  assert.match(navigation, /requiresDonations: true/)
  assert.match(navbar, /canAccessNavigationItem\(item, user, config\.donationsEnabled\)/)
  assert.match(mobileHeader, /canAccessNavigationItem\(item, user, config\.donationsEnabled\)/)
  assert.match(panel, /useInfoGetDonations/)
  assert.match(panel, /data\.totalAmount/)
  assert.match(panel, /data\.supportCount/)
  assert.match(panel, /data\.supporterCount/)
  assert.doesNotMatch(panel, /supporterEmail|orderId|paymentMethod|apiKey/)
  assert.match(settings, /type="password"|<PasswordInput/)
  assert.match(settings, /leave blank to keep/)
  assert.match(api, /path: `\/api\/donations`/)
})
