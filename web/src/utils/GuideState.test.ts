import { Window } from 'happy-dom'
import assert from 'node:assert/strict'
import test from 'node:test'
import {
  DEFAULT_GUIDE_PREFERENCES,
  GUIDE_TOUR_STEPS,
  GUIDE_VERSION,
  completeGuide,
  completeTeamGuide,
  guideStorageKey,
  guideTourTargetSelector,
  createGuideAccountHandoff,
  markGuideFeatureSeen,
  nextGuideStepForTarget,
  openGuide,
  parseGuidePreferences,
  pauseGuide,
  persistGuidePreferenceUpdate,
  resolveChallengeDeliveryGuide,
  resolveGuideIdentity,
  retainTeamGuideActivation,
  resumeGuideAfterAccountHandoff,
  resetGuideProgress,
  resolveTeamGuideAction,
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

test('guide identity waits through transient player-profile failures', () => {
  assert.equal(resolveGuideIdentity('player-id', 500), 'player-id')
  assert.equal(resolveGuideIdentity(undefined, 401), 'guest')
  assert.equal(resolveGuideIdentity(undefined, 500), null)
  assert.equal(resolveGuideIdentity(undefined), null)
})

test('team guide acknowledges an input click and then follows the enabled action', () => {
  assert.equal(resolveTeamGuideAction('team-create', undefined), 'choose')
  assert.equal(resolveTeamGuideAction('team-create-name', undefined), 'select-create-name')
  assert.equal(resolveTeamGuideAction('team-create-name', 'team-create-name'), 'type-create-name')
  assert.equal(resolveTeamGuideAction('team-join-code', undefined), 'select-join-code')
  assert.equal(resolveTeamGuideAction('team-join-code', 'team-join-code'), 'paste-join-code')
  assert.equal(resolveTeamGuideAction('team-create-submit', 'team-create-name'), 'submit-create')
  assert.equal(resolveTeamGuideAction('team-join-submit', 'team-join-code'), 'submit-join')
})

test('team guide forgets field activation when the tour closes or the target departs', () => {
  assert.equal(retainTeamGuideActivation('team-create-name', 'team-create-name', true), 'team-create-name')
  assert.equal(retainTeamGuideActivation('team-create-name', 'team-create-name', false), undefined)
  assert.equal(retainTeamGuideActivation(undefined, 'team-create-name', true), undefined)
  assert.equal(retainTeamGuideActivation('team-create-submit', 'team-create-name', true), undefined)
})

test('guide progress accepts only known feature identifiers without duplicates', () => {
  const parsed = parseGuidePreferences(
    JSON.stringify({
      interactiveEnabled: true,
      completedVersion: 2,
      seenFeatures: ['container-wsrx', 'unknown', 'container-wsrx'],
    })
  )
  assert.deepEqual(parsed.seenFeatures, ['container-wsrx'])

  const completed = completeGuide(parsed)
  assert.equal(completed.completedVersion, GUIDE_VERSION)
  assert.deepEqual(markGuideFeatureSeen(completed, 'event-vpn').seenFeatures, ['container-wsrx', 'event-vpn'])
  assert.deepEqual(resetGuideProgress(completed), {
    ...DEFAULT_GUIDE_PREFERENCES,
    activeTourStep: 'welcome',
  })
})

test('challenge delivery modes have independent one-time guide checkpoints', () => {
  const staticSeen = markGuideFeatureSeen(DEFAULT_GUIDE_PREFERENCES, 'static-challenge')
  const directSeen = markGuideFeatureSeen(staticSeen, 'container-direct')
  const proxySeen = markGuideFeatureSeen(directSeen, 'container-wsrx')
  const vpnSeen = markGuideFeatureSeen(proxySeen, 'container-vpn')

  assert.deepEqual(vpnSeen.seenFeatures, ['static-challenge', 'container-direct', 'container-wsrx', 'container-vpn'])
  assert.deepEqual(
    parseGuidePreferences(
      JSON.stringify({
        ...vpnSeen,
        seenFeatures: [...vpnSeen.seenFeatures, 'dynamic-container'],
      })
    ).seenFeatures,
    vpnSeen.seenFeatures
  )
})

test('challenge delivery guide follows the effective service path and VPN precedence', () => {
  assert.equal(
    resolveChallengeDeliveryGuide({
      staticChallenge: true,
      containerChallenge: false,
      eventVpnRequired: false,
      platformProxy: true,
    }),
    'static-challenge'
  )
  assert.equal(
    resolveChallengeDeliveryGuide({
      staticChallenge: false,
      containerChallenge: true,
      eventVpnRequired: false,
      platformProxy: false,
    }),
    'container-direct'
  )
  assert.equal(
    resolveChallengeDeliveryGuide({
      staticChallenge: false,
      containerChallenge: true,
      eventVpnRequired: false,
      platformProxy: true,
    }),
    'container-wsrx'
  )
  assert.equal(
    resolveChallengeDeliveryGuide({
      staticChallenge: false,
      containerChallenge: true,
      eventVpnRequired: true,
      platformProxy: true,
    }),
    'container-vpn'
  )
  assert.equal(
    resolveChallengeDeliveryGuide({
      staticChallenge: false,
      containerChallenge: false,
      eventVpnRequired: false,
      platformProxy: true,
    }),
    null
  )
})

test('active tour checkpoints survive navigation and reject unknown steps', () => {
  assert.equal(GUIDE_VERSION, 5)
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

test('guide checkpoints persist synchronously before a target can navigate away', () => {
  const order: string[] = []
  const next = persistGuidePreferenceUpdate(
    openGuide(DEFAULT_GUIDE_PREFERENCES),
    (current) => {
      order.push('update')
      return setGuideTourStep(current, 'account')
    },
    (serialized) => {
      order.push('persist')
      assert.equal(parseGuidePreferences(serialized).activeTourStep, 'account')
    }
  )
  order.push('navigate')

  assert.equal(next.activeTourStep, 'account')
  assert.deepEqual(order, ['update', 'persist', 'navigate'])
  assert.doesNotThrow(() =>
    persistGuidePreferenceUpdate(next, pauseGuide, () => {
      throw new Error('storage unavailable')
    })
  )
})

test('real tutorial actions advance only when they complete the current task', () => {
  assert.equal(nextGuideStepForTarget('welcome', 'more-navigation'), null)
  assert.equal(nextGuideStepForTarget('welcome', 'guide-navigation'), 'account')
  assert.equal(nextGuideStepForTarget('events', 'event-card'), null)
  assert.equal(nextGuideStepForTarget('events', 'event-challenges'), 'challenges')
  assert.equal(nextGuideStepForTarget('challenges', 'challenge-card'), 'connection')
  assert.equal(nextGuideStepForTarget('connection', 'instance-start'), null)
  assert.equal(nextGuideStepForTarget('connection', 'instance-entry'), 'submit')
  assert.equal(nextGuideStepForTarget('connection', 'instance-copy'), 'submit')
  assert.equal(nextGuideStepForTarget('connection', 'flag-submit'), 'submit')
  assert.equal(nextGuideStepForTarget('connection', 'challenge-material'), 'submit')
  assert.equal(nextGuideStepForTarget('submit', 'flag-submit'), null)
  assert.equal(nextGuideStepForTarget('team', 'team-create-name'), null)
  assert.equal(nextGuideStepForTarget('team', 'team-create-submit'), null)

  const teamStep = setGuideTourStep(DEFAULT_GUIDE_PREFERENCES, 'team')
  assert.equal(completeTeamGuide(teamStep).activeTourStep, 'events')
  const accountStep = setGuideTourStep(DEFAULT_GUIDE_PREFERENCES, 'account')
  assert.equal(completeTeamGuide(accountStep), accountStep)
})

test('every novice checkpoint resolves a page target before and after navigation', () => {
  const selector = (step: (typeof GUIDE_TOUR_STEPS)[number], pathname: string, signedIn = true) =>
    guideTourTargetSelector({ step, pathname, signedIn })

  for (const step of GUIDE_TOUR_STEPS) {
    assert.ok(selector(step, '/games').includes('data-guide'), `${step} needs a games-page fallback`)
  }

  assert.match(selector('account', '/games'), /account-menu/)
  assert.match(selector('account', '/account/profile'), /account-access/)
  assert.match(
    guideTourTargetSelector({ step: 'account', pathname: '/account/login', signedIn: false, preferOAuth: true }),
    /^\[data-guide="account-oauth"\]/
  )
  assert.match(selector('team', '/account/profile'), /team-navigation/)
  const teamSelector = selector('team', '/teams')
  assert.match(teamSelector, /team-create-workflow[^,]+team-create-name/)
  assert.match(teamSelector, /team-join-workflow[^,]+team-join-code/)
  assert.match(teamSelector, /team-create-workflow[^,]+team-create-submit[^,]+:not\(:disabled\)/)
  assert.match(teamSelector, /team-join-workflow[^,]+team-join-submit[^,]+:not\(:disabled\)/)
  assert.match(selector('events', '/teams'), /games-navigation/)
  assert.match(selector('events', '/games'), /event-card/)
  assert.match(selector('events', '/games/23'), /event-challenges/)
  assert.match(selector('challenges', '/games/23'), /challenge-navigation/)
  assert.match(selector('challenges', '/games/23/challenges'), /challenge-card/)
  assert.match(selector('connection', '/games/23/challenges'), /instance-start/)
  assert.match(selector('submit', '/games/23/challenges'), /flag-submit/)

  assert.match(
    guideTourTargetSelector({
      step: 'connection',
      pathname: '/games/23/challenges',
      signedIn: true,
      challengeFeature: 'static-challenge',
    }),
    /flag-submit/
  )
  assert.match(
    guideTourTargetSelector({
      step: 'connection',
      pathname: '/games/23/challenges',
      signedIn: true,
      challengeFeature: 'container-wsrx',
      instanceActive: true,
    }),
    /instance-entry/
  )
})

test('team setup cursor moves from the required input to the enabled submit action', async () => {
  const browser = new Window({ url: 'https://rsctf.test/teams' })
  const form = browser.document.createElement('form')
  form.dataset.guide = 'team-create-workflow'
  form.dataset.guideStage = 'input'
  const input = browser.document.createElement('input')
  input.dataset.guide = 'team-create-name'
  const submit = browser.document.createElement('button')
  submit.dataset.guide = 'team-create-submit'
  submit.disabled = true
  const launcher = browser.document.createElement('button')
  launcher.dataset.guide = 'team-create'
  form.append(input, submit)
  browser.document.body.append(form, launcher)

  const selector = guideTourTargetSelector({ step: 'team', pathname: '/teams', signedIn: true })
  const firstMatch = () =>
    selector
      .split(',')
      .map((candidate) => candidate.trim())
      .flatMap((candidate) => Array.from(browser.document.querySelectorAll<HTMLElement>(candidate)))[0]

  assert.equal(firstMatch(), input)
  form.dataset.guideStage = 'submit'
  submit.disabled = false
  assert.equal(firstMatch(), submit)

  await browser.happyDOM.close()
})

test('account sign-in handoff resumes a fresh authenticated guide at team setup', () => {
  const now = 1_800_000_000_000
  const resumed = resumeGuideAfterAccountHandoff(DEFAULT_GUIDE_PREFERENCES, createGuideAccountHandoff(now), now)
  assert.equal(resumed.activeTourStep, 'team')
  assert.equal(resumed.tourPaused, false)

  assert.equal(
    resumeGuideAfterAccountHandoff(
      DEFAULT_GUIDE_PREFERENCES,
      createGuideAccountHandoff(now - 2 * 60 * 60 * 1000 - 1),
      now
    ),
    DEFAULT_GUIDE_PREFERENCES
  )
  assert.equal(resumeGuideAfterAccountHandoff(DEFAULT_GUIDE_PREFERENCES, '{bad json', now), DEFAULT_GUIDE_PREFERENCES)
  assert.equal(
    resumeGuideAfterAccountHandoff(
      { ...DEFAULT_GUIDE_PREFERENCES, interactiveEnabled: false },
      createGuideAccountHandoff(now),
      now
    ).activeTourStep,
    null
  )
})
