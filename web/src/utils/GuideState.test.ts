import assert from 'node:assert/strict'
import test from 'node:test'
import {
  DEFAULT_GUIDE_PREFERENCES,
  GUIDE_VERSION,
  completeGuide,
  guideStorageKey,
  markGuideFeatureSeen,
  parseGuidePreferences,
  resetGuideProgress,
} from './GuideState'

test('guide preferences are account-scoped and fail closed to safe defaults', () => {
  assert.equal(guideStorageKey('a-user'), 'rsctf-player-guide:a-user')
  assert.equal(guideStorageKey(undefined), 'rsctf-player-guide:guest')
  assert.deepEqual(parseGuidePreferences('{broken'), DEFAULT_GUIDE_PREFERENCES)
  assert.deepEqual(parseGuidePreferences('{"interactiveEnabled":false,"completedVersion":-5}'), {
    interactiveEnabled: false,
    completedVersion: 0,
    seenFeatures: [],
  })
})

test('guide progress accepts only known feature identifiers without duplicates', () => {
  const parsed = parseGuidePreferences(
    JSON.stringify({
      interactiveEnabled: true,
      completedVersion: 2,
      seenFeatures: ['dynamic-container', 'unknown', 'dynamic-container'],
    })
  )
  assert.deepEqual(parsed.seenFeatures, ['dynamic-container'])

  const completed = completeGuide(parsed)
  assert.equal(completed.completedVersion, GUIDE_VERSION)
  assert.deepEqual(markGuideFeatureSeen(completed, 'event-vpn').seenFeatures, ['dynamic-container', 'event-vpn'])
  assert.deepEqual(resetGuideProgress(completed), DEFAULT_GUIDE_PREFERENCES)
})
