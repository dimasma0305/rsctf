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
  assert.match(entry, /proxyIssueNoInstanceCapability\(instanceEntry, \{ signal: controller\.signal \}\)/)
  assert.match(entry, /proxyIssueInstanceCapability\(instanceEntry, \{ signal: controller\.signal \}\)/)
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
  assert.match(entry, /setCapabilityExpiresAt\(expiresAt\)/)
  assert.match(entry, /CAPABILITY_REFRESH_SAFETY_MS/)
  assert.match(
    entry,
    /useServerClockTimeout\([\s\S]*?onRefreshProxyEntry\('automatic'\)[\s\S]*?capabilityExpiresAt[\s\S]*?CAPABILITY_REFRESH_SAFETY_MS/
  )
})

test('capability renewal prepares and verifies a replacement before switching the live entry', () => {
  const request = entry.match(/const launchCapabilityRequest = useCallback\([\s\S]*?const retryPendingPreparation/)?.[0]
  const handoff = entry.match(
    /useEffect\(\(\) => \{[\s\S]*?isWsrxReplacementReady\(pendingTraffic\)[\s\S]*?\n  \]\)/
  )?.[0]

  assert.ok(request)
  assert.ok(handoff)
  assert.match(request, /setPendingWsrxRemoteEntry\(candidate\)/)
  assert.match(request, /remoteEntryRef\.current &&\s+proxyModeRef\.current === 'wsrx'/)
  assert.doesNotMatch(request, /proxyModeRef\.current === 'wsrx' &&\s+wsrxStateRef\.current/)
  assert.doesNotMatch(request, /wsrx\.delete\(localTraffic\.local\)/)
  assert.match(entry, /remote: requestedRemoteEntry/)
  assert.match(entry, /watchPendingTunnel\(pendingWsrxRemoteEntry/)
  assert.match(handoff, /isWsrxReplacementReady\(pendingTraffic\)/)
  assert.match(handoff, /commitCapability\(\s+pendingWsrxRemoteEntry/)
  assert.match(entry, /scheduleTunnelDrain\(local, getWsrxListenerDrainDelay\(now, expiresAt\)\)/)
  assert.match(entry, /commitCapability\([\s\S]*?proxyEntryMode === 'wsrx',[\s\S]*?pendingTraffic\?\.local[\s\S]*?\)/)
  assert.match(
    entry,
    /shouldDeletePreparedWsrxListener\(replacementReady, preparedLocal, oldLocal\)[\s\S]*?wsrx\.delete\(preparedLocal\)/
  )
  assert.match(
    entry,
    /if \(!pendingWsrxRemoteEntry \|\| proxyEntryMode !== 'wsrx' \|\| wsrxState !== WsrxState\.Usable\) return[\s\S]*?watchPendingTunnel\(pendingWsrxRemoteEntry/
  )
})

test('capability renewal has one abortable owner with bounded retry and terminal 4xx handling', () => {
  assert.match(entry, /const renewalOwner = useRef\(false\)/)
  assert.match(entry, /const capabilityGeneration = useRef\(0\)/)
  assert.match(
    entry,
    /if \(!isPlatformProxy \|\| !instanceEntry \|\| \(acquireOwnership && renewalOwner\.current\)\) return/
  )
  assert.match(entry, /const controller = new AbortController\(\)/)
  assert.match(entry, /capabilityAbort\.current\?\.abort\(\)/)
  assert.match(entry, /generation !== capabilityGeneration\.current/)
  assert.match(entry, /attempt < MAX_CAPABILITY_REQUEST_ATTEMPTS/)
  assert.match(entry, /isRetryableWsrxCapabilityStatus\(status\)/)
  assert.match(entry, /headers\?\.get\?\.\('retry-after'\)/)
  assert.match(entry, /signal\.addEventListener\('abort', finish, \{ once: true \}\)/)
  assert.match(entry, /getWsrxCapabilityNextBatchDelay\(now, oldExpiresAt, retryDelay\)/)
  assert.match(
    entry,
    /scheduleRenewalTimer\(\(\) => \{[\s\S]*?launchCapabilityRequestRef\.current\('automatic', false\)/
  )
})

test('late local-listener resolutions are removed after their active or pending owner is superseded', () => {
  assert.match(entry, /const componentMounted = useRef\(true\)/)
  assert.match(entry, /const activeEntryGeneration = useRef\(0\)/)
  assert.match(entry, /const added = await wsrx\.add\(/)
  assert.match(entry, /capabilityGeneration\.current === requestedGeneration/)
  assert.match(entry, /activeEntryGeneration\.current === requestedGeneration/)
  assert.match(entry, /remoteEntryRef\.current === requestedRemoteEntry/)
  assert.equal(entry.match(/await wsrx\.delete\(added\.local\)\.catch\(\(\) => undefined\)/g)?.length, 2)
})

test('extension availability follows initial and corrected server clock samples', () => {
  assert.match(entry, /isInstanceExtensionWindowOpen\([\s\S]*?getServerNowMilliseconds\(\)[\s\S]*?\)/)
  assert.match(entry, /const authoritativeClockOffset = useServerClockOffset\(\)/)
  assert.match(entry, /\[authoritativeClockOffset,[\s\S]*?\]/)
  assert.match(entry, /if \(!extensionWindowOpen\) enableExtend\.cancel\(\)/)
  assert.doesNotMatch(entry, /dayjs\(context\.closeTime \?\? 0\)\.diff\(dayjs\(\)\)/)
})
