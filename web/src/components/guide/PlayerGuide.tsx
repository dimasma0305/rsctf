import { Badge, Button, Group, List, Progress, Stack, Text } from '@mantine/core'
import { mdiArrowLeft, mdiArrowRight, mdiCheck, mdiOpenInNew } from '@mdi/js'
import { Icon } from '@mdi/react'
import {
  Dispatch,
  FC,
  PropsWithChildren,
  SetStateAction,
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react'
import { useTranslation } from 'react-i18next'
import { useLocation, useNavigate } from 'react-router'
import { GuideSpotlightModal } from '@Components/guide/GuideSpotlightModal'
import {
  GUIDE_VERSION,
  GUIDE_TOUR_STEPS,
  GuideFeature,
  GuidePreferences,
  GuideTourStep,
  completeGuide,
  guideStorageKey,
  markGuideFeatureSeen,
  nextGuideStepForTarget,
  openGuide,
  parseGuidePreferences,
  pauseGuide,
  resetGuideProgress,
  setGuideTourStep,
} from '@Utils/GuideState'
import { useConfig } from '@Hooks/useConfig'
import { useUser } from '@Hooks/useUser'
import { ContainerPortMappingType } from '@Api'
import classes from '@Styles/PlayerGuide.module.css'

interface GuideFeatureContext {
  eventVpnRequired?: boolean
}

interface PendingFeature {
  feature: GuideFeature
  context: GuideFeatureContext
}

interface PlayerGuideContextValue {
  preferences: GuidePreferences
  ready: boolean
  startGuide: () => void
  setInteractiveEnabled: (enabled: boolean) => void
  resetGuide: () => void
  introduceFeature: (feature: GuideFeature, context?: GuideFeatureContext) => void
}

const PlayerGuideContext = createContext<PlayerGuideContextValue | null>(null)

export const usePlayerGuide = () => {
  const context = useContext(PlayerGuideContext)
  if (!context) throw new Error('usePlayerGuide must be used inside PlayerGuideProvider')
  return context
}

export const useFeatureGuide = (feature: GuideFeature, active: boolean, context: GuideFeatureContext = {}) => {
  const guide = usePlayerGuide()
  const eventVpnRequired = context.eventVpnRequired

  useEffect(() => {
    if (active) guide.introduceFeature(feature, { eventVpnRequired })
  }, [active, eventVpnRequired, feature, guide.introduceFeature])
}

interface TourStep {
  id: GuideTourStep
  title: string
  body: string
  note: string
  path?: string
  pathLabel?: string
  targetSelector?: string
}

interface AccessibleGuideModalProps extends PropsWithChildren {
  opened: boolean
  onClose: () => void
  title: string
  closeLabel: string
  size: string
  overlayOpacity: number
  targetSelector?: string
  onTargetActivate?: (target: string | undefined) => void
}

const AccessibleGuideModal: FC<AccessibleGuideModalProps> = ({
  opened,
  onClose,
  title,
  closeLabel,
  size,
  overlayOpacity,
  targetSelector,
  onTargetActivate,
  children,
}) => (
  <GuideSpotlightModal
    opened={opened}
    onClose={onClose}
    size={size}
    title={title}
    closeLabel={closeLabel}
    overlayOpacity={overlayOpacity}
    targetSelector={targetSelector}
    onTargetActivate={onTargetActivate}
  >
    {children}
  </GuideSpotlightModal>
)

const preferenceUpdater = (
  storageKey: string,
  setPreferences: Dispatch<SetStateAction<GuidePreferences>>,
  update: (current: GuidePreferences) => GuidePreferences
) => {
  setPreferences((current) => {
    const next = update(current)
    try {
      window.localStorage.setItem(storageKey, JSON.stringify(next))
    } catch {
      // Private-browsing and storage-quota failures must not break navigation.
      // The in-memory preference still applies for this tab.
    }
    return next
  })
}

const loadPreferences = (storageKey: string) => {
  try {
    return parseGuidePreferences(window.localStorage.getItem(storageKey))
  } catch {
    return parseGuidePreferences(null)
  }
}

export const PlayerGuideProvider: FC<PropsWithChildren> = ({ children }) => {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const location = useLocation()
  const { config } = useConfig()
  const { user, error: userError } = useUser()
  const identity = user?.userId ?? (userError ? 'guest' : null)
  const storageKey = guideStorageKey(identity)
  const [loadedKey, setLoadedKey] = useState<string | null>(null)
  const [preferences, setPreferences] = useState<GuidePreferences>(() => parseGuidePreferences(null))
  const [pendingFeature, setPendingFeature] = useState<PendingFeature | null>(null)
  const autoStartedKeys = useRef(new Set<string>())
  const ready = identity !== null && loadedKey === storageKey

  useEffect(() => {
    if (identity === null) return
    setPreferences(loadPreferences(storageKey))
    setLoadedKey(storageKey)
    setPendingFeature(null)
  }, [identity, storageKey])

  const updatePreferences = useCallback(
    (update: (current: GuidePreferences) => GuidePreferences) => {
      if (!ready) return
      preferenceUpdater(storageKey, setPreferences, update)
    },
    [ready, storageKey]
  )

  const setInteractiveEnabled = useCallback(
    (enabled: boolean) => {
      updatePreferences((current) => ({
        ...current,
        interactiveEnabled: enabled,
        activeTourStep: enabled ? current.activeTourStep : null,
        tourPaused: enabled ? current.tourPaused : false,
      }))
      if (!enabled) {
        setPendingFeature(null)
      }
    },
    [updatePreferences]
  )

  const startGuide = useCallback(() => {
    updatePreferences(openGuide)
  }, [updatePreferences])

  const resetGuide = useCallback(() => {
    updatePreferences(resetGuideProgress)
    setPendingFeature(null)
  }, [updatePreferences])

  const introduceFeature = useCallback(
    (feature: GuideFeature, context: GuideFeatureContext = {}) => {
      if (!ready || !preferences.interactiveEnabled || preferences.seenFeatures.includes(feature)) return
      setPendingFeature((current) => current ?? { feature, context })
    },
    [preferences.interactiveEnabled, preferences.seenFeatures, ready]
  )

  useEffect(() => {
    const isSafeEntryPage = location.pathname === '/' || location.pathname === '/games'
    if (
      !ready ||
      !isSafeEntryPage ||
      !preferences.interactiveEnabled ||
      preferences.completedVersion >= GUIDE_VERSION ||
      preferences.activeTourStep !== null ||
      autoStartedKeys.current.has(storageKey)
    ) {
      return
    }
    autoStartedKeys.current.add(storageKey)
    updatePreferences(openGuide)
  }, [
    location.pathname,
    preferences.activeTourStep,
    preferences.completedVersion,
    preferences.interactiveEnabled,
    ready,
    storageKey,
    updatePreferences,
  ])

  const providerNames = [config.enableGoogleAuth ? 'Google' : null, config.enableDiscordAuth ? 'Discord' : null].filter(
    (provider): provider is string => Boolean(provider)
  )
  const accountBody = user
    ? t('guide.tour.account.signed_in', 'You are signed in as {{name}}. Select Next to set up your team.', {
        name: user.userName ?? t('common.tab.account.title', 'your account'),
      })
    : config.allowRegister === false
      ? t(
          'guide.tour.account.closed',
          'Registration is closed. Sign in with an existing account or ask an organizer for access.'
        )
      : config.allowPasswordRegistration === false
        ? t(
            'guide.tour.account.oauth',
            'Register or sign in with {{providers}}. You do not need a separate platform password.',
            {
              providers:
                providerNames.join(' or ') || t('guide.tour.account.configured_provider', 'a configured provider'),
            }
          )
        : t(
            'guide.tour.account.password',
            'Register with email and password{{oauth}}, then keep using the same account.',
            {
              oauth: providerNames.length
                ? t('guide.tour.account.oauth_suffix', ', or use {{providers}}', {
                    providers: providerNames.join(' / '),
                  })
                : '',
            }
          )
  const connectionBody =
    config.portMapping === ContainerPortMappingType.PlatformProxy
      ? t(
          'guide.tour.connection.proxy',
          'Select Start instance. When it is ready, open Connection tools and use the local address it shows.'
        )
      : t(
          'guide.tour.connection.direct',
          'Select Start instance. When it is ready, connect to the displayed host and port.'
        )
  const isGameDetailPage = /^\/games\/\d+$/.test(location.pathname)
  const isTeamPage = location.pathname === '/teams'
  const isChallengePage = location.pathname === '/challenges' || /^\/games\/\d+\/challenges$/.test(location.pathname)

  const steps = useMemo<TourStep[]>(
    () => [
      {
        id: 'welcome',
        title: t('guide.tour.welcome.title', 'Learn the playground'),
        body: t(
          'guide.tour.welcome.body',
          'Follow the highlighted control and do one task at a time. The guide stays with you when the page changes.'
        ),
        note: t('guide.tour.welcome.note', 'It never joins, starts, or submits anything for you.'),
        targetSelector: '[data-guide="guide-navigation"], [data-guide="more-navigation"]',
      },
      {
        id: 'account',
        title: t('guide.tour.account.title', user ? 'Your player account' : 'Sign in once'),
        body: accountBody,
        note: config.emailConfirmationRequired
          ? t('guide.tour.account.verify', 'Email confirmation is required before the account can play.')
          : t('guide.tour.account.ready', 'Your teams, solves, and event access stay with this account.'),
        path: user ? '/account/profile' : '/account/login',
        pathLabel: user
          ? t('guide.tour.account.open_profile', 'Open profile')
          : t('guide.tour.account.open_login', 'Open login'),
        targetSelector: location.pathname.startsWith('/account/') ? '[data-guide="account-access"]' : undefined,
      },
      {
        id: 'team',
        title: t('guide.tour.team.title', 'Create or join a team'),
        body: user
          ? isTeamPage
            ? t(
                'guide.tour.team.destination_body',
                'Choose Create team. If a captain sent you an invite code, choose Join team instead.'
              )
            : t('guide.tour.team.body', 'Open Teams, then create a team or join one with an invite code.')
          : t('guide.tour.team.guest_body', 'Sign in first. Events are entered with a team.'),
        note: t('guide.tour.team.note', 'One person creates the team; everyone else joins with its invite code.'),
        path: user && !isTeamPage ? '/teams' : !user ? '/account/login' : undefined,
        pathLabel: user ? t('guide.tour.team.open', 'Open teams') : t('guide.tour.team.login_first', 'Sign in first'),
        targetSelector: isTeamPage ? '[data-guide="team-create"], [data-guide="team-join"]' : undefined,
      },
      {
        id: 'events',
        title: t('guide.tour.events.title', isGameDetailPage ? 'Join this event' : 'Choose an event'),
        body: isGameDetailPage
          ? t(
              'guide.tour.events.detail_body',
              'Review the schedule and rules, then use the highlighted action to join with your team.'
            )
          : t(
              'guide.tour.events.body',
              'Select the highlighted event card. Check its schedule and rules before joining.'
            ),
        note: isGameDetailPage
          ? t('guide.tour.events.detail_note', 'If your team is already approved, open Challenges.')
          : t('guide.tour.events.note', 'Some organizers review a team before approving it.'),
        path: isGameDetailPage ? undefined : '/games',
        pathLabel: t('guide.tour.events.open', 'Open games'),
        targetSelector:
          location.pathname === '/games'
            ? '[data-guide="event-card"], [data-guide="games-search"]'
            : isGameDetailPage
              ? '[data-guide="event-join"]:not(:disabled), [data-guide="event-challenges"]'
              : undefined,
      },
      {
        id: 'challenges',
        title: t('guide.tour.challenges.title', 'Open a challenge'),
        body: t(
          'guide.tour.challenges.body',
          'Open a challenge card. Read its description and download any attachment before solving.'
        ),
        note: t('guide.tour.challenges.note', 'My challenges only contains events your team joined.'),
        path: isChallengePage ? undefined : user ? '/challenges' : '/games',
        pathLabel: user
          ? t('guide.tour.challenges.open', 'Open my challenges')
          : t('guide.tour.challenges.login_first', 'Browse events first'),
        targetSelector: user && isChallengePage ? '[data-guide="challenge-card"]' : undefined,
      },
      {
        id: 'connection',
        title: t('guide.tour.connection.title', 'Start and connect'),
        body: connectionBody,
        note: t('guide.tour.connection.note', 'Static challenges skip this step. VPN-only events use their event VPN.'),
        targetSelector: '[data-guide="instance-start"], [data-guide="instance-entry"], [data-guide="challenge-card"]',
      },
      {
        id: 'submit',
        title: t('guide.tour.submit.title', 'Submit the flag'),
        body: t(
          'guide.tour.submit.body',
          'Paste the exact flag into the highlighted Flag field, then select Submit. A correct verdict adds the score.'
        ),
        note: t('guide.tour.submit.note', 'Keep flags, accounts, VPN profiles, and instance addresses private.'),
        path: user && !isChallengePage ? '/challenges' : !user ? '/games' : undefined,
        pathLabel: user
          ? t('guide.tour.submit.open_challenges', 'Open my challenges')
          : t('guide.tour.submit.browse_events', 'Browse events'),
        targetSelector: '[data-guide="flag-submit"], [data-guide="challenge-card"]',
      },
    ],
    [
      accountBody,
      config.emailConfirmationRequired,
      connectionBody,
      isChallengePage,
      isGameDetailPage,
      isTeamPage,
      location.pathname,
      t,
      user,
    ]
  )
  const activeStepIndex = preferences.activeTourStep ? GUIDE_TOUR_STEPS.indexOf(preferences.activeTourStep) : -1
  const stepIndex = activeStepIndex >= 0 ? activeStepIndex : 0
  const step = steps[stepIndex]
  const tourOpen = ready && preferences.activeTourStep !== null && !preferences.tourPaused
  const destinationPath = step.path?.split(/[?#]/, 1)[0]
  const atStepDestination = Boolean(destinationPath && location.pathname === destinationPath)
  const completeTour = () => {
    updatePreferences(completeGuide)
  }

  const moveToStep = useCallback(
    (index: number) => {
      const next = steps[Math.min(steps.length - 1, Math.max(0, index))]
      updatePreferences((current) => setGuideTourStep(current, next.id))
    },
    [steps, updatePreferences]
  )

  const onTourTargetActivate = useCallback(
    (target: string | undefined) => {
      const nextStep = nextGuideStepForTarget(step.id, target)
      if (nextStep) updatePreferences((current) => setGuideTourStep(current, nextStep))
    },
    [step.id, updatePreferences]
  )

  const dismissFeature = () => {
    if (pendingFeature) updatePreferences((current) => markGuideFeatureSeen(current, pendingFeature.feature))
    setPendingFeature(null)
  }

  const value = useMemo<PlayerGuideContextValue>(
    () => ({ preferences, ready, startGuide, setInteractiveEnabled, resetGuide, introduceFeature }),
    [introduceFeature, preferences, ready, resetGuide, setInteractiveEnabled, startGuide]
  )

  return (
    <PlayerGuideContext.Provider value={value}>
      {children}

      <AccessibleGuideModal
        opened={tourOpen}
        onClose={() => updatePreferences(pauseGuide)}
        title={step.title}
        size="min(21rem, calc(100vw - 1rem))"
        closeLabel={t('guide.tour.pause', 'Pause guide')}
        overlayOpacity={0.58}
        targetSelector={step.targetSelector}
        onTargetActivate={onTourTargetActivate}
      >
        <Stack gap="sm" className={classes.tourBody}>
          <Badge variant="light" size="sm" className={classes.stepBadge}>
            {t('guide.tour.progress', 'Step {{current}} of {{total}}', {
              current: stepIndex + 1,
              total: steps.length,
            })}
          </Badge>
          <Progress
            size="xs"
            value={((stepIndex + 1) / steps.length) * 100}
            aria-label={t('guide.tour.progress', 'Step {{current}} of {{total}}', {
              current: stepIndex + 1,
              total: steps.length,
            })}
          />
          <Stack gap="xs" role="status" aria-live="polite" aria-atomic="true">
            <Text size="sm">{step.body}</Text>
            <Text size="sm" c="dimmed" className={classes.note}>
              {step.note}
            </Text>
          </Stack>
          {step.path && !atStepDestination && (
            <Button
              variant="light"
              leftSection={<Icon path={mdiOpenInNew} size={0.72} aria-hidden="true" />}
              onClick={() => navigate(step.path!)}
              className={classes.guideAction}
            >
              {step.pathLabel}
            </Button>
          )}
          {atStepDestination && (
            <Text size="sm" c="dimmed" role="status" aria-live="polite" className={classes.destinationNote}>
              {t(
                'guide.tour.destination_ready',
                'Use the highlighted control. When you finish that action, select Next.'
              )}
            </Text>
          )}
          <Group justify="space-between" gap="xs" wrap="nowrap" className={classes.tourFooter}>
            <Button variant="subtle" color="gray" onClick={() => setInteractiveEnabled(false)}>
              {t('guide.tour.disable', 'Stop guide')}
            </Button>
            <Group gap={4} wrap="nowrap">
              <Button
                variant="default"
                disabled={stepIndex === 0}
                leftSection={<Icon path={mdiArrowLeft} size={0.7} aria-hidden="true" />}
                onClick={() => moveToStep(stepIndex - 1)}
              >
                {t('common.pagination.previous', 'Previous')}
              </Button>
              {stepIndex === steps.length - 1 ? (
                <Button leftSection={<Icon path={mdiCheck} size={0.7} aria-hidden="true" />} onClick={completeTour}>
                  {t('guide.tour.finish', 'Finish')}
                </Button>
              ) : (
                <Button
                  rightSection={<Icon path={mdiArrowRight} size={0.7} aria-hidden="true" />}
                  onClick={() => moveToStep(stepIndex + 1)}
                >
                  {t('common.pagination.next', 'Next')}
                </Button>
              )}
            </Group>
          </Group>
        </Stack>
      </AccessibleGuideModal>

      <AccessibleGuideModal
        opened={Boolean(pendingFeature) && !tourOpen && ready}
        onClose={dismissFeature}
        title={
          pendingFeature?.feature === 'event-vpn'
            ? t('guide.feature.vpn.title', 'New: this event requires its VPN')
            : t('guide.feature.container.title', 'New: this challenge starts an instance')
        }
        size="min(23rem, calc(100vw - 1rem))"
        closeLabel={t('guide.feature.dismiss', 'Dismiss this tip')}
        overlayOpacity={0.58}
        targetSelector={
          pendingFeature?.feature === 'event-vpn'
            ? '[data-guide="event-vpn-download"]'
            : '[data-guide="instance-start"], [data-guide="instance-entry"]'
        }
      >
        {pendingFeature && (
          <Stack gap="md">
            {pendingFeature.feature === 'event-vpn' ? (
              <List type="ordered" spacing="sm" className={classes.featureList}>
                <List.Item>
                  {t('guide.feature.vpn.download', 'Download the VPN profile from the event page.')}
                </List.Item>
                <List.Item>
                  {t(
                    'guide.feature.vpn.connect',
                    'Import it into WireGuard and connect before opening challenge ports.'
                  )}
                </List.Item>
                <List.Item>
                  {t('guide.feature.vpn.private', 'Keep the event profile private; it identifies your event access.')}
                </List.Item>
              </List>
            ) : (
              <List type="ordered" spacing="sm" className={classes.featureList}>
                <List.Item>
                  {t(
                    'guide.feature.container.start',
                    'Select Start instance. The first start may build or pull the image on demand, so wait for the success message.'
                  )}
                </List.Item>
                <List.Item>
                  {pendingFeature.context.eventVpnRequired
                    ? t(
                        'guide.feature.container.vpn_connection',
                        'This event is VPN-only. Connect its WireGuard profile, then use the displayed host and port.'
                      )
                    : config.portMapping === ContainerPortMappingType.PlatformProxy
                      ? t(
                          'guide.feature.container.proxy_connection',
                          'The platform proxy creates a local connection address for you. Use the address shown after the instance is ready.'
                        )
                      : t(
                          'guide.feature.container.direct_connection',
                          'Connect to the host and port shown after the instance is ready.'
                        )}
                </List.Item>
                <List.Item>
                  {t(
                    'guide.feature.container.cleanup',
                    'Extend it near expiry if you still need it, or destroy it when finished to release resources.'
                  )}
                </List.Item>
              </List>
            )}
            <Group justify="space-between" gap="sm" wrap="wrap-reverse">
              <Button variant="subtle" color="gray" onClick={() => setInteractiveEnabled(false)}>
                {t('guide.feature.disable', 'Turn off future tips')}
              </Button>
              <Group gap="xs">
                <Button
                  variant="default"
                  onClick={() => {
                    dismissFeature()
                    navigate('/guide')
                  }}
                >
                  {t('guide.feature.full_guide', 'Full guide')}
                </Button>
                <Button onClick={dismissFeature}>{t('guide.feature.understood', 'Got it')}</Button>
              </Group>
            </Group>
          </Stack>
        )}
      </AccessibleGuideModal>
    </PlayerGuideContext.Provider>
  )
}
