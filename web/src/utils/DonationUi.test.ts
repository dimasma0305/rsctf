import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const source = (path: string) => readFileSync(path, 'utf8')

test('global notifications use the unobtrusive bottom-right anchor', () => {
  const app = source('src/App.tsx')
  const css = source('src/styles/App.css')
  assert.match(app, /<Notifications position="bottom-right"/)
  assert.doesNotMatch(app, /<Notifications position="top-right"/)
  assert.match(css, /\.app-notifications\[data-position\^='bottom-'\]\s*\{/)
  assert.doesNotMatch(css, /\n\s*\.app-notifications\s*\{\s*\n\s*bottom:/)
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
  assert.match(donations, /donateUrl=\{config\.donationUrl\}/)
  assert.match(donations, /if \(loading\) return <WithNavBar isLoading/)
  assert.match(donations, /!config\.donationsEnabled/)
  assert.match(navigation, /link: '\/donations'/)
  assert.match(navigation, /requiresDonations: true/)
  assert.match(navbar, /canAccessNavigationItem\(item, user, config\.donationsEnabled\)/)
  assert.match(mobileHeader, /canAccessNavigationItem\(item, user, config\.donationsEnabled\)/)
  assert.match(panel, /useInfoGetDonations/)
  assert.doesNotMatch(panel, /data\.totalAmount|total_received|balance_note/)
  assert.match(panel, /data\.supportCount/)
  assert.match(panel, /data\.supporterCount/)
  assert.match(panel, /href=\{donateUrl\}/)
  assert.match(panel, /target="_blank"/)
  assert.match(panel, /rel="noopener noreferrer"/)
  assert.doesNotMatch(panel, /supporterEmail|orderId|paymentMethod|apiKey/)
  assert.match(settings, /type="password"|<PasswordInput/)
  assert.match(settings, /leave blank to keep/)
  assert.match(settings, /donations\.donate_url/)
  assert.match(settings, /donateUrl/)
  assert.match(api, /path: `\/api\/donations`/)
})
