import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const entry = readFileSync('src/components/InstanceEntry.tsx', 'utf8')
const shared = readFileSync('src/utils/Shared.tsx', 'utf8')

test('the WSRX action row remains single-line and has room for retry, copy, and open', () => {
  assert.match(entry, /<Group gap=\{2\} wrap="nowrap">/)
  assert.match(entry, /rightSectionWidth=\{isPlatformProxy \? '7\.75rem' : '5rem'\}/)
  assert.match(entry, /wsrx\.button\.retry_tunnel/)
})

test('WSRX receives an authenticated narrow capability without exposing the browser session', () => {
  assert.match(entry, /proxyIssueNoInstanceCapability\(instanceEntry\)/)
  assert.match(entry, /proxyIssueInstanceCapability\(instanceEntry\)/)
  assert.match(entry, /getProxyEntry\(instanceEntry, isPreview, response\.data\.token\)/)
  assert.match(shared, /capability \? `\$\{url\}\?capability=\$\{encodeURIComponent\(capability\)\}` : url/)
  assert.doesNotMatch(entry, /RSCTF_Token|document\.cookie|Authorization/)
})

test('admin previews explain the WSRX requirement and block unusable entries', () => {
  assert.doesNotMatch(entry, /isPlatformProxy &&\s*!isPreview/)
  assert.match(entry, /disabled=\{!canUseEntry\}/)
  assert.match(entry, /phase === 'ready'/)
  assert.match(entry, /role="status" aria-live="polite"/)
})

test('the disabled open-web action remains a valid button', () => {
  assert.match(entry, /onClick=\{onOpenEntry\}/)
  assert.match(entry, /if \(!canOpenEntry\) return/)
  assert.match(entry, /window\.open\(`http:\/\/\$\{webEntry\}`, '_blank', 'noopener,noreferrer'\)/)
  assert.doesNotMatch(entry, /component="a"/)
})

test('players can explicitly switch between a local netcat address and the scoped WSS URL', () => {
  assert.match(entry, /if \(!isPlatformProxy \|\| !instanceEntry\) return/)
  assert.doesNotMatch(entry, /if \(!isWsrxUsable \|\| !instanceEntry\) return/)
  assert.match(entry, /<SegmentedControl/)
  assert.match(entry, /value: 'wsrx'/)
  assert.match(entry, /value: 'wss'/)
  assert.match(entry, /isWssMode \? wsrxRemoteEntry : localEntry/)
  assert.match(entry, /clipBoard\.copy\(entry\)/)
  assert.match(entry, /disabled=\{!canOpenEntry\}/)
})

test('the scoped WSS capability is renewed before it can leave a stale local listener', () => {
  assert.match(entry, /setCapabilityExpiresAt\(response\.data\.expiresAt\)/)
  assert.match(entry, /CAPABILITY_REFRESH_SAFETY_MS/)
  assert.match(entry, /setTimeout\(\(\) => void onRefreshProxyEntry\(\), refreshIn\)/)
})

test('extension availability is initialized from the corrected server clock', () => {
  assert.match(entry, /isInstanceExtensionWindowOpen\([\s\S]*?getServerNowMilliseconds\(\)[\s\S]*?\)/)
  assert.doesNotMatch(entry, /dayjs\(context\.closeTime \?\? 0\)\.diff\(dayjs\(\)\)/)
})
