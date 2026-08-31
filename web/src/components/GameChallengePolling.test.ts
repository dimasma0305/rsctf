import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'

const modal = readFileSync('src/components/GameChallengeModal.tsx', 'utf8')
const panel = readFileSync('src/components/ChallengePanel.tsx', 'utf8')
const shell = readFileSync('src/components/ChallengeModal.tsx', 'utf8')
const hook = readFileSync('src/hooks/useChallengePolling.ts', 'utf8')
const apiContract = readFileSync('src/Api.ts', 'utf8')

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
  assert.match(hook, /failureCount\.current = 0[\s\S]*setPausedKey\(null\)[\s\S]*cancel\(\)/)
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
})

test('the challenge-detail BYOC ownership contract reaches the A&D panel before a service row exists', () => {
  const challengeDetailContract = apiContract.slice(
    apiContract.indexOf('export interface ChallengeDetailModel'),
    apiContract.indexOf('export interface ChallengeSolverPreviewModel')
  )
  assert.match(challengeDetailContract, /adSelfHosted\?: boolean/)
  assert.match(shell, /selfHosted=\{challenge\?\.adSelfHosted === true\}/)
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
