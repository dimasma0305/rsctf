export const GUIDE_VERSION = 5
export const GUIDE_STORAGE_PREFIX = 'rsctf-player-guide'
export const GUIDE_ACCOUNT_HANDOFF_KEY = `${GUIDE_STORAGE_PREFIX}:account-handoff`
const GUIDE_ACCOUNT_HANDOFF_TTL = 2 * 60 * 60 * 1000

export const GUIDE_FEATURES = [
  'static-challenge',
  'container-direct',
  'container-wsrx',
  'container-vpn',
  'event-vpn',
] as const
export const GUIDE_TOUR_STEPS = ['welcome', 'account', 'team', 'events', 'challenges', 'connection', 'submit'] as const

export type GuideFeature = (typeof GUIDE_FEATURES)[number]
export type GuideTourStep = (typeof GUIDE_TOUR_STEPS)[number]

export type TeamGuideAction =
  | 'choose'
  | 'select-create-name'
  | 'type-create-name'
  | 'select-join-code'
  | 'paste-join-code'
  | 'submit-create'
  | 'submit-join'

export const resolveTeamGuideAction = (
  activeTarget: string | undefined,
  activatedTarget: string | undefined
): TeamGuideAction => {
  switch (activeTarget) {
    case 'team-create-name':
      return activatedTarget === activeTarget ? 'type-create-name' : 'select-create-name'
    case 'team-join-code':
      return activatedTarget === activeTarget ? 'paste-join-code' : 'select-join-code'
    case 'team-create-submit':
      return 'submit-create'
    case 'team-join-submit':
      return 'submit-join'
    default:
      return 'choose'
  }
}

export const retainTeamGuideActivation = (
  activeTarget: string | undefined,
  activatedTarget: string | undefined,
  tourOpen: boolean
) => (tourOpen && activeTarget === activatedTarget ? activatedTarget : undefined)

export const resolveGuideIdentity = (userId?: string | null, userErrorStatus?: number): string | null => {
  if (userId) return userId
  return userErrorStatus === 401 ? 'guest' : null
}

/** Keep an established scope while SWR transiently clears the profile result. */
export const retainGuideIdentity = (current: string | null, resolved: string | null): string | null =>
  resolved ?? current

interface GuideTourTargetContext {
  step: GuideTourStep
  pathname: string
  signedIn: boolean
  preferOAuth?: boolean
  challengeFeature?: GuideFeature | null
  instanceActive?: boolean
}

interface ChallengeDeliveryGuideContext {
  staticChallenge: boolean
  containerChallenge: boolean
  eventVpnRequired: boolean
  platformProxy: boolean
}

export const resolveChallengeDeliveryGuide = ({
  staticChallenge,
  containerChallenge,
  eventVpnRequired,
  platformProxy,
}: ChallengeDeliveryGuideContext): GuideFeature | null => {
  if (staticChallenge) return 'static-challenge'
  if (!containerChallenge) return null
  if (eventVpnRequired) return 'container-vpn'
  return platformProxy ? 'container-wsrx' : 'container-direct'
}

const ACCOUNT_NAVIGATION_TARGETS =
  '[data-guide="account-profile"], [data-guide="account-login"], [data-guide="account-menu"], [data-guide="more-navigation"]'
const TEAM_NAVIGATION_TARGETS = '[data-guide="team-navigation"], [data-guide="more-navigation"]'
const GAMES_NAVIGATION_TARGETS = '[data-guide="games-navigation"], [data-guide="more-navigation"]'
const CHALLENGE_NAVIGATION_TARGETS = '[data-guide="challenge-navigation"], [data-guide="more-navigation"]'

/**
 * Resolve a real, reachable page control for every tour checkpoint. Selector
 * order represents the action nearest to completion: an open dialog comes
 * before its launcher, and a page control comes before a navigation fallback.
 */
export const guideTourTargetSelector = ({
  step,
  pathname,
  signedIn,
  preferOAuth,
  challengeFeature,
  instanceActive,
}: GuideTourTargetContext) => {
  const isAccountPage = pathname.startsWith('/account/')
  const isTeamPage = pathname === '/teams'
  const isGamesPage = pathname === '/games'
  const isGameDetailPage = /^\/games\/\d+$/.test(pathname)
  const isChallengePage = pathname === '/challenges' || /^\/games\/\d+\/challenges$/.test(pathname)
  const accountPageTargets = preferOAuth
    ? '[data-guide="account-oauth"], [data-guide="account-access"]'
    : '[data-guide="account-access"], [data-guide="account-oauth"]'

  switch (step) {
    case 'welcome':
      return '[data-guide="guide-navigation"], [data-guide="more-navigation"]'
    case 'account':
      return isAccountPage ? `${accountPageTargets}, ${ACCOUNT_NAVIGATION_TARGETS}` : ACCOUNT_NAVIGATION_TARGETS
    case 'team':
      if (!signedIn)
        return isAccountPage ? `${accountPageTargets}, ${ACCOUNT_NAVIGATION_TARGETS}` : ACCOUNT_NAVIGATION_TARGETS
      return isTeamPage
        ? '[data-guide="team-create-workflow"][data-guide-stage="input"] [data-guide="team-create-name"], [data-guide="team-join-workflow"][data-guide-stage="input"] [data-guide="team-join-code"], [data-guide="team-create-workflow"][data-guide-stage="submit"] [data-guide="team-create-submit"]:not(:disabled), [data-guide="team-join-workflow"][data-guide-stage="submit"] [data-guide="team-join-submit"]:not(:disabled), [data-guide="team-create"], [data-guide="team-join"], [data-guide="team-navigation"]'
        : TEAM_NAVIGATION_TARGETS
    case 'events':
      if (isGameDetailPage) {
        return '[data-guide="event-join-confirm"], [data-guide="event-join-team"], [data-guide="event-join-division"], [data-guide="event-join-code"], [data-guide="event-join-submit"], [data-guide="event-challenges"], [data-guide="event-join"], [data-guide="event-briefing"]'
      }
      return isGamesPage
        ? '[data-guide="event-card"], [data-guide="games-search"], [data-guide="games-navigation"]'
        : GAMES_NAVIGATION_TARGETS
    case 'challenges':
      if (isChallengePage) {
        return '[data-guide="challenge-card"], [data-guide="challenge-navigation"], [data-guide="more-navigation"]'
      }
      return signedIn ? CHALLENGE_NAVIGATION_TARGETS : `[data-guide="event-card"], ${GAMES_NAVIGATION_TARGETS}`
    case 'connection':
      if (challengeFeature === 'static-challenge') {
        return '[data-guide="flag-submit"], [data-guide="challenge-material"]'
      }
      if (challengeFeature?.startsWith('container-')) {
        return instanceActive
          ? '[data-guide="instance-entry"], [data-guide="instance-copy"], [data-guide="flag-submit"]'
          : '[data-guide="instance-start"], [data-guide="instance-entry"], [data-guide="flag-submit"]'
      }
      if (isChallengePage) {
        return '[data-guide="instance-start"], [data-guide="instance-entry"], [data-guide="flag-submit"], [data-guide="challenge-material"], [data-guide="challenge-card"]'
      }
      return signedIn ? CHALLENGE_NAVIGATION_TARGETS : GAMES_NAVIGATION_TARGETS
    case 'submit':
      if (challengeFeature || isChallengePage) {
        return '[data-guide="flag-submit"], [data-guide="challenge-material"], [data-guide="challenge-card"], [data-guide="challenge-navigation"]'
      }
      return signedIn ? CHALLENGE_NAVIGATION_TARGETS : GAMES_NAVIGATION_TARGETS
  }
}

const GUIDE_TARGET_ADVANCE: Partial<Record<GuideTourStep, readonly string[]>> = {
  welcome: ['guide-navigation'],
  events: ['event-challenges'],
  challenges: ['challenge-card'],
  connection: ['instance-entry', 'instance-copy', 'flag-submit', 'challenge-material'],
}

export interface GuidePreferences {
  interactiveEnabled: boolean
  completedVersion: number
  seenFeatures: GuideFeature[]
  activeTourStep: GuideTourStep | null
  tourPaused: boolean
}

export const persistGuidePreferenceUpdate = (
  current: GuidePreferences,
  update: (preferences: GuidePreferences) => GuidePreferences,
  persist: (serialized: string) => void
) => {
  const next = update(current)
  try {
    persist(JSON.stringify(next))
  } catch {
    // Storage failures must not block the in-memory tutorial state.
  }
  return next
}

export const DEFAULT_GUIDE_PREFERENCES: GuidePreferences = {
  interactiveEnabled: true,
  completedVersion: 0,
  seenFeatures: [],
  activeTourStep: null,
  tourPaused: false,
}

const featureSet = new Set<string>(GUIDE_FEATURES)
const tourStepSet = new Set<string>(GUIDE_TOUR_STEPS)

export const guideStorageKey = (identity?: string | null) => `${GUIDE_STORAGE_PREFIX}:${identity?.trim() || 'guest'}`

export const parseGuidePreferences = (value: string | null | undefined): GuidePreferences => {
  if (!value) return { ...DEFAULT_GUIDE_PREFERENCES }

  try {
    const parsed = JSON.parse(value) as Partial<GuidePreferences>
    const seenFeatures = Array.isArray(parsed.seenFeatures)
      ? [...new Set(parsed.seenFeatures.filter((feature): feature is GuideFeature => featureSet.has(feature)))]
      : []
    const activeTourStep =
      typeof parsed.activeTourStep === 'string' && tourStepSet.has(parsed.activeTourStep)
        ? (parsed.activeTourStep as GuideTourStep)
        : null

    return {
      interactiveEnabled:
        typeof parsed.interactiveEnabled === 'boolean'
          ? parsed.interactiveEnabled
          : DEFAULT_GUIDE_PREFERENCES.interactiveEnabled,
      completedVersion:
        typeof parsed.completedVersion === 'number' && Number.isSafeInteger(parsed.completedVersion)
          ? Math.max(0, parsed.completedVersion)
          : DEFAULT_GUIDE_PREFERENCES.completedVersion,
      seenFeatures,
      activeTourStep,
      tourPaused: activeTourStep !== null && parsed.tourPaused === true,
    }
  } catch {
    return { ...DEFAULT_GUIDE_PREFERENCES }
  }
}

export const createGuideAccountHandoff = (createdAt = Date.now()) =>
  JSON.stringify({ version: GUIDE_VERSION, createdAt })

export const resumeGuideAfterAccountHandoff = (
  preferences: GuidePreferences,
  serializedHandoff: string | null | undefined,
  now = Date.now()
): GuidePreferences => {
  if (
    !serializedHandoff ||
    !preferences.interactiveEnabled ||
    preferences.activeTourStep !== null ||
    preferences.completedVersion >= GUIDE_VERSION
  ) {
    return preferences
  }

  try {
    const handoff = JSON.parse(serializedHandoff) as { version?: unknown; createdAt?: unknown }
    if (
      handoff.version !== GUIDE_VERSION ||
      typeof handoff.createdAt !== 'number' ||
      !Number.isFinite(handoff.createdAt) ||
      handoff.createdAt > now + 60_000 ||
      now - handoff.createdAt > GUIDE_ACCOUNT_HANDOFF_TTL
    ) {
      return preferences
    }
  } catch {
    return preferences
  }

  return setGuideTourStep(preferences, 'team')
}

export const completeGuide = (preferences: GuidePreferences): GuidePreferences => ({
  ...preferences,
  completedVersion: GUIDE_VERSION,
  activeTourStep: null,
  tourPaused: false,
})

export const openGuide = (preferences: GuidePreferences): GuidePreferences => ({
  ...preferences,
  interactiveEnabled: true,
  activeTourStep: preferences.activeTourStep ?? GUIDE_TOUR_STEPS[0],
  tourPaused: false,
})

export const pauseGuide = (preferences: GuidePreferences): GuidePreferences => ({
  ...preferences,
  tourPaused: preferences.activeTourStep !== null,
})

export const setGuideTourStep = (preferences: GuidePreferences, activeTourStep: GuideTourStep): GuidePreferences => ({
  ...preferences,
  activeTourStep,
  tourPaused: false,
})

export const nextGuideStepForTarget = (currentStep: GuideTourStep, target: string | undefined) => {
  if (!target || !GUIDE_TARGET_ADVANCE[currentStep]?.includes(target)) return null
  const currentIndex = GUIDE_TOUR_STEPS.indexOf(currentStep)
  return GUIDE_TOUR_STEPS[currentIndex + 1] ?? null
}

export const completeTeamGuide = (preferences: GuidePreferences): GuidePreferences =>
  preferences.activeTourStep === 'team' ? setGuideTourStep(preferences, 'events') : preferences

export const markGuideFeatureSeen = (preferences: GuidePreferences, feature: GuideFeature): GuidePreferences => ({
  ...preferences,
  seenFeatures: preferences.seenFeatures.includes(feature)
    ? preferences.seenFeatures
    : [...preferences.seenFeatures, feature],
})

export const resetGuideProgress = (preferences: GuidePreferences): GuidePreferences => ({
  ...preferences,
  interactiveEnabled: true,
  completedVersion: 0,
  seenFeatures: [],
  activeTourStep: GUIDE_TOUR_STEPS[0],
  tourPaused: false,
})
