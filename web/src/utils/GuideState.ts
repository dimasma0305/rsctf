export const GUIDE_VERSION = 1
export const GUIDE_STORAGE_PREFIX = 'rsctf-player-guide'

export const GUIDE_FEATURES = ['dynamic-container', 'event-vpn'] as const

export type GuideFeature = (typeof GUIDE_FEATURES)[number]

export interface GuidePreferences {
  interactiveEnabled: boolean
  completedVersion: number
  seenFeatures: GuideFeature[]
}

export const DEFAULT_GUIDE_PREFERENCES: GuidePreferences = {
  interactiveEnabled: true,
  completedVersion: 0,
  seenFeatures: [],
}

const featureSet = new Set<string>(GUIDE_FEATURES)

export const guideStorageKey = (identity?: string | null) => `${GUIDE_STORAGE_PREFIX}:${identity?.trim() || 'guest'}`

export const parseGuidePreferences = (value: string | null | undefined): GuidePreferences => {
  if (!value) return { ...DEFAULT_GUIDE_PREFERENCES }

  try {
    const parsed = JSON.parse(value) as Partial<GuidePreferences>
    const seenFeatures = Array.isArray(parsed.seenFeatures)
      ? [...new Set(parsed.seenFeatures.filter((feature): feature is GuideFeature => featureSet.has(feature)))]
      : []

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
    }
  } catch {
    return { ...DEFAULT_GUIDE_PREFERENCES }
  }
}

export const completeGuide = (preferences: GuidePreferences): GuidePreferences => ({
  ...preferences,
  completedVersion: GUIDE_VERSION,
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
})
