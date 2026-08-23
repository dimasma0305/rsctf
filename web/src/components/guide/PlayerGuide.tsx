import { Badge, Button, Group, List, Progress, Stack, Text, ThemeIcon, Title } from '@mantine/core'
import { mdiArrowLeft, mdiArrowRight, mdiCheck, mdiFlagVariantOutline, mdiOpenInNew } from '@mdi/js'
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
  GuideFeature,
  GuidePreferences,
  completeGuide,
  guideStorageKey,
  markGuideFeatureSeen,
  parseGuidePreferences,
  resetGuideProgress,
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
  id: string
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
}

const AccessibleGuideModal: FC<AccessibleGuideModalProps> = ({
  opened,
  onClose,
  title,
  closeLabel,
  size,
  overlayOpacity,
  targetSelector,
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
  const [tourOpen, setTourOpen] = useState(false)
  const [stepIndex, setStepIndex] = useState(0)
  const [pendingFeature, setPendingFeature] = useState<PendingFeature | null>(null)
  const autoStartedKeys = useRef(new Set<string>())
  const ready = identity !== null && loadedKey === storageKey

  useEffect(() => {
    if (identity === null) return
    setPreferences(loadPreferences(storageKey))
    setLoadedKey(storageKey)
    setTourOpen(false)
    setPendingFeature(null)
    setStepIndex(0)
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
      updatePreferences((current) => ({ ...current, interactiveEnabled: enabled }))
      if (!enabled) {
        setTourOpen(false)
        setPendingFeature(null)
      }
    },
    [updatePreferences]
  )

  const startGuide = useCallback(() => {
    setInteractiveEnabled(true)
    setStepIndex(0)
    setTourOpen(true)
  }, [setInteractiveEnabled])

  const resetGuide = useCallback(() => {
    updatePreferences(resetGuideProgress)
    setPendingFeature(null)
    setStepIndex(0)
    setTourOpen(true)
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
      autoStartedKeys.current.has(storageKey)
    ) {
      return
    }
    autoStartedKeys.current.add(storageKey)
    setTourOpen(true)
  }, [location.pathname, preferences.completedVersion, preferences.interactiveEnabled, ready, storageKey])

  const providerNames = [config.enableGoogleAuth ? 'Google' : null, config.enableDiscordAuth ? 'Discord' : null].filter(
    (provider): provider is string => Boolean(provider)
  )
  const accountBody = user
    ? t(
        'guide.tour.account.signed_in',
        'You are signed in as {{name}}. Your teams and event memberships follow this account.',
        {
          name: user.userName ?? t('common.tab.account.title', 'your account'),
        }
      )
    : config.allowRegister === false
      ? t(
          'guide.tour.account.closed',
          'Public registration is closed on this platform. Sign in with an existing account or ask an organizer for access.'
        )
      : config.allowPasswordRegistration === false
        ? t(
            'guide.tour.account.oauth',
            'This platform uses OAuth-only registration. Continue with {{providers}}; no separate RSCTF password is created.',
            {
              providers:
                providerNames.join(' or ') || t('guide.tour.account.configured_provider', 'a configured provider'),
            }
          )
        : t(
            'guide.tour.account.password',
            'Create an account with email and password{{oauth}}. Use the same account whenever you return.',
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
          'Container challenges use the platform connection proxy. Start the instance, wait until it is ready, then use the local address shown by the connection tool.'
        )
      : t(
          'guide.tour.connection.direct',
          'Container challenges expose a host and port directly. Start the instance, wait until it is ready, then connect to the displayed address.'
        )

  const steps = useMemo<TourStep[]>(
    () => [
      {
        id: 'welcome',
        title: t('guide.tour.welcome.title', 'Welcome to {{platform}}', { platform: config.title || 'RSCTF' }),
        body: t(
          'guide.tour.welcome.body',
          'This short, interactive walkthrough follows the same path as a real player. You can pause it now and restart it from Guide at any time.'
        ),
        note: t('guide.tour.welcome.note', 'Nothing is submitted or changed while you view this guide.'),
        targetSelector: '[data-guide="guide-navigation"], [data-guide="more-navigation"]',
      },
      {
        id: 'account',
        title: t('guide.tour.account.title', 'Use one player account'),
        body: accountBody,
        note: config.emailConfirmationRequired
          ? t('guide.tour.account.verify', 'Email confirmation is required before the account can play.')
          : t('guide.tour.account.ready', 'After sign-in, create or join a team before entering an event.'),
        path: user ? '/account/profile' : '/account/login',
        pathLabel: user
          ? t('guide.tour.account.open_profile', 'Open profile')
          : t('guide.tour.account.open_login', 'Open login'),
        targetSelector: '[data-guide="account-menu"], [data-guide="more-navigation"]',
      },
      {
        id: 'events',
        title: t('guide.tour.events.title', 'Find and join an event'),
        body: t(
          'guide.tour.events.body',
          'Games shows every visible event. Search by title or ID, then use the participation filter to separate joined and not-yet-joined events.'
        ),
        note: t(
          'guide.tour.events.note',
          'Open an event, choose your team, and submit the join request. Some organizers review requests before accepting them.'
        ),
        path: '/games',
        pathLabel: t('guide.tour.events.open', 'Open games'),
        targetSelector:
          location.pathname === '/games' ? '[data-guide="games-search"]' : '[data-guide="games-navigation"]',
      },
      {
        id: 'challenges',
        title: t('guide.tour.challenges.title', 'Use your challenge workspace'),
        body: t(
          'guide.tour.challenges.body',
          'My challenges searches across events you have joined. Filters help you narrow by category, challenge type, and solved status.'
        ),
        note: t(
          'guide.tour.challenges.note',
          'The catalog never reveals challenges from events you have not joined, hidden events, or events that have not started.'
        ),
        path: user ? '/challenges' : '/games',
        pathLabel: user
          ? t('guide.tour.challenges.open', 'Open my challenges')
          : t('guide.tour.challenges.login_first', 'Browse events first'),
        targetSelector:
          user && location.pathname === '/challenges'
            ? '[data-guide="challenge-filters"]'
            : user
              ? '[data-guide="challenge-navigation"]'
              : '[data-guide="games-navigation"]',
      },
      {
        id: 'connection',
        title: t('guide.tour.connection.title', 'Start and connect safely'),
        body: connectionBody,
        note: t(
          'guide.tour.connection.note',
          'VPN-only events override the platform default: download that event’s VPN profile and use the event-provided port instructions.'
        ),
        targetSelector:
          '[data-guide="connection-tools"], [data-guide="instance-start"], [data-guide="instance-entry"], [data-guide="games-navigation"]',
      },
      {
        id: 'submit',
        title: t('guide.tour.submit.title', 'Submit the flag'),
        body: t(
          'guide.tour.submit.body',
          'Solve the challenge, paste the exact flag into the submission field, and wait for the verdict. A correct flag updates your team score.'
        ),
        note: t(
          'guide.tour.submit.note',
          'Do not share flags, accounts, VPN profiles, or private instance addresses. Event rules and organizer notices take priority.'
        ),
        path: '/guide',
        pathLabel: t('guide.tour.submit.full_guide', 'Read the full guide'),
        targetSelector:
          '[data-guide="challenge-navigation"], [data-guide="flag-submit"], [data-guide="games-navigation"]',
      },
    ],
    [accountBody, config.emailConfirmationRequired, config.title, connectionBody, location.pathname, t, user]
  )
  const step = steps[Math.min(stepIndex, steps.length - 1)]
  const completeTour = () => {
    updatePreferences(completeGuide)
    setTourOpen(false)
    setStepIndex(0)
  }

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
        opened={tourOpen && ready}
        onClose={() => setTourOpen(false)}
        title={t('guide.tour.dialog_title', 'Interactive player guide')}
        size="min(36rem, calc(100vw - 1.5rem))"
        closeLabel={t('guide.tour.pause', 'Pause guide')}
        overlayOpacity={0.72}
        targetSelector={step.targetSelector}
      >
        <Stack gap="lg">
          <Group justify="space-between" align="center" wrap="nowrap">
            <Badge variant="light">
              {t('guide.tour.progress', 'Step {{current}} of {{total}}', {
                current: stepIndex + 1,
                total: steps.length,
              })}
            </Badge>
            <Text size="xs" c="dimmed">
              {config.title || 'RSCTF'}
            </Text>
          </Group>
          <Progress
            value={((stepIndex + 1) / steps.length) * 100}
            aria-label={t('guide.tour.progress', 'Step {{current}} of {{total}}', {
              current: stepIndex + 1,
              total: steps.length,
            })}
          />
          <Stack gap="xs" role="status" aria-live="polite" aria-atomic="true">
            <ThemeIcon size={46} radius="xl" variant="light" aria-hidden="true">
              <Icon path={stepIndex === steps.length - 1 ? mdiCheck : mdiFlagVariantOutline} size={1.05} />
            </ThemeIcon>
            <Title order={2} size="h3">
              {step.title}
            </Title>
            <Text>{step.body}</Text>
            <Text size="sm" c="dimmed" className={classes.note}>
              {step.note}
            </Text>
          </Stack>
          {step.path && (
            <Button
              variant="light"
              leftSection={<Icon path={mdiOpenInNew} size={0.72} aria-hidden="true" />}
              onClick={() => navigate(step.path!)}
            >
              {step.pathLabel}
            </Button>
          )}
          <Group justify="space-between" gap="sm" wrap="wrap-reverse">
            <Button variant="subtle" color="gray" onClick={() => setInteractiveEnabled(false)}>
              {t('guide.tour.disable', 'Turn off interactive guide')}
            </Button>
            <Group gap="xs">
              <Button
                variant="default"
                disabled={stepIndex === 0}
                leftSection={<Icon path={mdiArrowLeft} size={0.7} aria-hidden="true" />}
                onClick={() => setStepIndex((current) => Math.max(0, current - 1))}
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
                  onClick={() => setStepIndex((current) => Math.min(steps.length - 1, current + 1))}
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
        size="min(34rem, calc(100vw - 1.5rem))"
        closeLabel={t('guide.feature.dismiss', 'Dismiss this tip')}
        overlayOpacity={0.65}
        targetSelector={
          pendingFeature?.feature === 'event-vpn'
            ? '[data-guide="event-vpn-download"]'
            : '[data-guide="instance-start"], [data-guide="instance-entry"]'
        }
      >
        {pendingFeature && (
          <Stack gap="lg">
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
