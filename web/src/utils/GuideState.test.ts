import assert from 'node:assert/strict'
import test from 'node:test'
import {
  DEFAULT_GUIDE_PREFERENCES,
  GUIDE_TOUR_STEPS,
  GUIDE_VERSION,
  completeGuide,
  guideStorageKey,
  markGuideFeatureSeen,
  nextGuideStepForTarget,
  openGuide,
  parseGuidePreferences,
  pauseGuide,
  resetGuideProgress,
  setGuideTourStep,
} from './GuideState'

test('guide preferences are account-scoped and fail closed to safe defaults', () => {
  assert.equal(guideStorageKey('a-user'), 'rsctf-player-guide:a-user')
  assert.equal(guideStorageKey(undefined), 'rsctf-player-guide:guest')
  assert.deepEqual(parseGuidePreferences('{broken'), DEFAULT_GUIDE_PREFERENCES)
  assert.deepEqual(parseGuidePreferences('{"interactiveEnabled":false,"completedVersion":-5}'), {
    interactiveEnabled: false,
    completedVersion: 0,
    seenFeatures: [],
    activeTourStep: null,
    tourPaused: false,
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
  assert.deepEqual(resetGuideProgress(completed), {
    ...DEFAULT_GUIDE_PREFERENCES,
    activeTourStep: 'welcome',
  })
})

test('active tour checkpoints survive navigation and reject unknown steps', () => {
  assert.equal(GUIDE_VERSION, 4)
  assert.deepEqual(GUIDE_TOUR_STEPS, ['welcome', 'account', 'team', 'events', 'challenges', 'connection', 'submit'])

  const opened = openGuide(DEFAULT_GUIDE_PREFERENCES)
  assert.equal(opened.activeTourStep, GUIDE_TOUR_STEPS[0])
  assert.equal(opened.tourPaused, false)

  const destination = setGuideTourStep(opened, 'events')
  const restored = parseGuidePreferences(JSON.stringify(pauseGuide(destination)))
  assert.equal(restored.activeTourStep, 'events')
  assert.equal(restored.tourPaused, true)
  assert.equal(openGuide(restored).activeTourStep, 'events')

  assert.deepEqual(
    parseGuidePreferences(
      JSON.stringify({
        ...destination,
        activeTourStep: 'not-a-real-step',
        tourPaused: true,
      })
    ),
    {
      ...destination,
      activeTourStep: null,
      tourPaused: false,
    }
  )
  assert.equal(completeGuide(destination).activeTourStep, null)
})

test('real tutorial actions advance only when they complete the current task', () => {
  assert.equal(nextGuideStepForTarget('welcome', 'more-navigation'), null)
  assert.equal(nextGuideStepForTarget('welcome', 'guide-navigation'), 'account')
  assert.equal(nextGuideStepForTarget('events', 'event-card'), null)
  assert.equal(nextGuideStepForTarget('events', 'event-challenges'), 'challenges')
  assert.equal(nextGuideStepForTarget('challenges', 'challenge-card'), 'connection')
  assert.equal(nextGuideStepForTarget('connection', 'instance-start'), null)
  assert.equal(nextGuideStepForTarget('connection', 'instance-entry'), 'submit')
  assert.equal(nextGuideStepForTarget('submit', 'flag-submit'), null)
})
