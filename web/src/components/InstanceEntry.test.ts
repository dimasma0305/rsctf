import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const entry = readFileSync('src/components/InstanceEntry.tsx', 'utf8')
const shared = readFileSync('src/utils/Shared.tsx', 'utf8')

test('the optional WSRX action row remains single-line and has room for all three actions', () => {
  assert.match(entry, /<Group gap=\{2\} wrap="nowrap">/)
  assert.match(entry, /rightSectionWidth=\{hasWsrxTunnel \? '7\.75rem' : '5rem'\}/)
})

test('WSRX receives an authenticated narrow capability without exposing the browser session', () => {
  assert.match(entry, /proxyIssueNoInstanceCapability\(instanceEntry\)/)
  assert.match(entry, /proxyIssueInstanceCapability\(instanceEntry\)/)
  assert.match(entry, /getProxyEntry\(instanceEntry, isPreview, response\.data\.token\)/)
  assert.match(shared, /capability \? `\$\{url\}\?capability=\$\{encodeURIComponent\(capability\)\}` : url/)
  assert.doesNotMatch(entry, /RSCTF_Token|document\.cookie|Authorization/)
})
