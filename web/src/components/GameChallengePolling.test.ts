import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const modal = readFileSync('src/components/GameChallengeModal.tsx', 'utf8')
const panel = readFileSync('src/components/ChallengePanel.tsx', 'utf8')
const shell = readFileSync('src/components/ChallengeModal.tsx', 'utf8')
const hook = readFileSync('src/hooks/useChallengePolling.ts', 'utf8')
const apiContract = readFileSync('src/Api.ts', 'utf8')
const vpnDownload = readFileSync('src/utils/EventVpnDownload.ts', 'utf8')

test('closed challenge modals own no detail, solver, A&D, or KotH polling key', () => {
  assert.match(
    modal,
    /active: readEnabled,[\s\S]*refreshInterval: 0,[\s\S]*revalidateOnFocus: false,[\s\S]*revalidateOnReconnect: false/
  )
  assert.doesNotMatch(modal, /refreshInterval: 120 \* 1000/)
  assert.match(modal, /solvers\/page\?count=20&skip=0`[\s\S]*active: readEnabled/)
  assert.match(
    modal,
    /const readEnabled = shouldReadChallenge\(modalProps\.opened, challengeOwned, gameId, challengeId\)/
  )
  assert.match(shell, /KothChallengePanel[\s\S]*active=\{Boolean\(modalProps\.opened\)\}/)
  assert.match(shell, /AdChallengePanel[\s\S]*active=\{Boolean\(modalProps\.opened\)\}/)
  assert.match(hook, /const liveKey = active && key \? key : null/)
  assert.match(hook, /revalidateOnFocus: revalidateOnFocus && pausedKey !== key/)
  assert.match(hook, /revalidateOnReconnect: revalidateOnReconnect && pausedKey !== key/)
  assert.match(hook, /failureCount\.current = 0[\s\S]*setPausedKey\(null\)[\s\S]*return cancel/)
  assert.doesNotMatch(
    hook,
    /failureCount\.current = 0[\s\S]*setPausedKey\(null\)[\s\S]*cancel\(\)[\s\S]*return cancel/
  )
})

test('an open challenge detail is mutation-driven instead of periodically replacing cached content with a transient error', () => {
  const detailOwner = modal.slice(
    modal.indexOf('useChallengePolling<ChallengeDetailModel>'),
    modal.indexOf('const solverRequest')
  )
  assert.match(detailOwner, /refreshInterval: 0/)
  assert.match(detailOwner, /revalidateOnFocus: false/)
  assert.match(detailOwner, /revalidateOnReconnect: false/)
  assert.match(modal, /confirmCreatedInstance\(res\.data, mutate\)/)
  assert.match(modal, /refresh: mutate/)
  assert.match(
    modal,
    /loadError=\{eventVpnDisconnected \|\| challenge === undefined \? challengePollError : undefined\}/
  )
  assert.match(
    modal,
    /refreshError=\{challenge !== undefined && !eventVpnDisconnected \? challengePollError : undefined\}/
  )
  assert.match(
    modal,
    /solverError=\{challenge !== undefined \? pollErrorMessage\(solverError, 'solvers'\) : undefined\}/
  )
})

test('detail and solver diagnostics share one recovery owner and carry safe request identities', () => {
  assert.match(modal, /const recoveryOwner = useMemo\(createChallengeRecoveryOwner, \[\]\)/)
  assert.equal([...modal.matchAll(/recoveryOwner,/g)].length, 2)
  assert.match(modal, /recoveryKey: 'challenge-detail'/)
  assert.match(modal, /recoveryKey: 'challenge-solvers'/)
  assert.equal([...modal.matchAll(/headers: challengeRequestHeaders\(requestId\)/g)].length, 2)
  assert.match(modal, /captureChallengeReadFailure\(error, 'challenge', requestId\)/)
  assert.match(modal, /captureChallengeReadFailure\(error, 'solvers', requestId\)/)
  assert.match(modal, /Promise\.allSettled\(reads\)/)
})

test('solver-only failure remains secondary and typed failures do not collapse into one message', () => {
  assert.match(shell, /solverError[\s\S]*color="yellow"/)
  assert.match(modal, /error\.kind === 'disconnected'/)
  assert.match(modal, /error\.kind === 'rate-limited'/)
  assert.match(modal, /status === 401/)
  assert.match(modal, /status === 403/)
  assert.match(modal, /status === 429/)
  assert.match(modal, /status !== null && status >= 500/)
  assert.match(modal, /Request reference/)
})

test('a typed disconnected VPN read exposes setup before retrying protected challenge material', () => {
  assert.match(
    modal,
    /eventVpnRequired && isEventVpnAccessError\(challengeError\) && challengeError\.kind === 'disconnected'/
  )
  assert.match(modal, /onDownloadEventVpn=\{eventVpnDisconnected \? onDownloadEventVpn : undefined\}/)
  assert.match(shell, /eventVpnDisconnected[\s\S]*Download event VPN[\s\S]*I’m connected — retry/)
  assert.match(shell, /Challenge targets stay hidden until the VPN connection is verified/)
  assert.match(vpnDownload, /gameVpnConfig\(gameId\)/)
  assert.match(vpnDownload, /anchor\.download = `rsctf-event-\$\{gameId\}\.conf`/)
  assert.match(vpnDownload, /finally[\s\S]*anchor\.remove\(\)[\s\S]*URL\.revokeObjectURL\(url\)/)
})

test('a disconnected event VPN gives the challenge list an actionable setup state', () => {
  assert.match(
    panel,
    /game\?\.vpnAccessRequired && isEventVpnAccessError\(teamInfoError\) && teamInfoError\.kind === 'disconnected'/
  )
  assert.match(panel, /Connect to the event VPN to load challenges/)
  assert.match(panel, /Download event VPN/)
  assert.match(panel, /I’m connected — retry/)
  assert.match(panel, /allowEventVpnReconnectRetry\(numId\)[\s\S]*mutateTeamInfo\(\)/)
})

test('ambiguous container failures retain their retry identity while terminal client failures clear it', () => {
  const terminalPolicy = modal.slice(
    modal.indexOf('const isTerminalContainerOperationError'),
    modal.indexOf('const retainContainerOperation')
  )
  assert.match(terminalPolicy, /status !== null/)
  assert.match(terminalPolicy, /status >= 400 && status < 500/)
  assert.match(terminalPolicy, /status !== 409 && status !== 429/)
  assert.doesNotMatch(terminalPolicy, /status !== undefined/)
})

test('the challenge-detail BYOC ownership contract reaches the A&D panel before a service row exists', () => {
  const challengeDetailContract = apiContract.slice(
    apiContract.indexOf('export interface ChallengeDetailModel'),
    apiContract.indexOf('export interface ChallengeSolverPreviewModel')
  )
  assert.match(challengeDetailContract, /adSelfHosted\?: boolean/)
  assert.match(shell, /selfHosted=\{challenge\?\.adSelfHosted === true\}/)
  assert.match(apiContract, /export enum AdServiceDeliveryState/)
  assert.match(apiContract, /deliveryState: AdServiceDeliveryState/)
})

test('challenge list no longer polls the nonexistent review summary route', () => {
  assert.doesNotMatch(panel, /Reviews\/Summary/)
  assert.doesNotMatch(panel, /ratingSWRFetcher/)
})

test('modal polling suspends for hidden or offline pages and has bounded retries', () => {
  assert.match(hook, /refreshWhenHidden: false/)
  assert.match(hook, /refreshWhenOffline: false/)
  assert.match(hook, /config\.isVisible\(\)/)
  assert.match(hook, /config\.isOnline\(\)/)
  assert.match(hook, /failureCount\.current >= MAX_CHALLENGE_POLL_RETRIES/)
})
