export const GUIDE_VERSION = 3
export const GUIDE_STORAGE_PREFIX = 'rsctf-player-guide'

export const GUIDE_FEATURES = ['dynamic-container', 'event-vpn'] as const
export const GUIDE_TOUR_STEPS = ['welcome', 'account', 'team', 'events', 'challenges', 'connection', 'submit'] as const

export type GuideFeature = (typeof GUIDE_FEATURES)[number]
export type GuideTourStep = (typeof GUIDE_TOUR_STEPS)[number]

export interface GuidePreferences {
  interactiveEnabled: boolean
  completedVersion: number
  seenFeatures: GuideFeature[]
  activeTourStep: GuideTourStep | null
  tourPaused: boolean
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
