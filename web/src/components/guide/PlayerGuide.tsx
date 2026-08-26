import { Button, Group, Stack, Text } from '@mantine/core'
import {
  mdiArrowLeft,
  mdiArrowRight,
  mdiCheck,
  mdiCursorDefaultClickOutline,
  mdiKeyboardOutline,
  mdiOpenInNew,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import {
  FC,
  PropsWithChildren,
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
  GUIDE_ACCOUNT_HANDOFF_KEY,
  GuideFeature,
  GuidePreferences,
  GuideTourStep,
  completeGuide,
  completeTeamGuide,
  createGuideAccountHandoff,
  guideStorageKey,
  guideTourTargetSelector,
  markGuideFeatureSeen,
  nextGuideStepForTarget,
  openGuide,
  parseGuidePreferences,
  pauseGuide,
  persistGuidePreferenceUpdate,
  resolveGuideIdentity,
  resetGuideProgress,
  resolveTeamGuideAction,
  resumeGuideAfterAccountHandoff,
  setGuideTourStep,
} from '@Utils/GuideState'
import { useConfig } from '@Hooks/useConfig'
import { useUser } from '@Hooks/useUser'
import { ContainerPortMappingType } from '@Api'
import classes from '@Styles/PlayerGuide.module.css'

interface GuideFeatureContext {
  eventVpnRequired?: boolean
  hasAttachment?: boolean
  instanceActive?: boolean
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
  completeTeamSetup: () => void
  introduceFeature: (feature: GuideFeature, context?: GuideFeatureContext) => void
}

const PlayerGuideContext = createContext<PlayerGuideContextValue | null>(null)

export const usePlayerGuide = () => {
  const context = useContext(PlayerGuideContext)
  if (!context) throw new Error('usePlayerGuide must be used inside PlayerGuideProvider')
  return context
}

export const useFeatureGuide = (feature: GuideFeature | null, active: boolean, context: GuideFeatureContext = {}) => {
  const guide = usePlayerGuide()
  const eventVpnRequired = context.eventVpnRequired
  const hasAttachment = context.hasAttachment
  const instanceActive = context.instanceActive

  useEffect(() => {
    if (active && feature) guide.introduceFeature(feature, { eventVpnRequired, hasAttachment, instanceActive })
  }, [active, eventVpnRequired, feature, guide.introduceFeature, hasAttachment, instanceActive])
}

interface FeatureStep {
  id: string
  title: string
  body: string
  note?: string
  command?: string
  targetSelector?: string
  advanceOnActivate?: boolean
}

interface TourStep {
  id: GuideTourStep
  title: string
  body: string
  note: string
  path?: string
  pathLabel?: string
  targetSelector?: string
  requiresTargetActivation?: boolean
  targetPrompt?: string
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
  onTargetChange?: (target: string | undefined) => void
  showTargetCursor?: boolean
  progress?: {
    current: number
    total: number
    label: string
  }
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
  onTargetChange,
  showTargetCursor,
  progress,
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
    onTargetChange={onTargetChange}
    showTargetCursor={showTargetCursor}
    progress={progress}
  >
    {children}
  </GuideSpotlightModal>
)

const GuideTargetPrompt: FC<PropsWithChildren<{ keyboardEntry?: boolean }>> = ({ children, keyboardEntry }) => (
  <Group gap="xs" wrap="nowrap" className={classes.targetPrompt} role="status" aria-live="polite">
    <Icon
      path={keyboardEntry ? mdiKeyboardOutline : mdiCursorDefaultClickOutline}
      size={0.82}
      className={classes.targetPromptIcon}
      aria-hidden="true"
    />
    <Text size="xs" fw={650}>
      {children}
    </Text>
  </Group>
)

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
  const identity = resolveGuideIdentity(user?.userId, userError?.status)
  const storageKey = guideStorageKey(identity)
  const [loadedKey, setLoadedKey] = useState<string | null>(null)
  const [preferences, setPreferences] = useState<GuidePreferences>(() => parseGuidePreferences(null))
  const preferencesRef = useRef(preferences)
  const [pendingFeature, setPendingFeature] = useState<PendingFeature | null>(null)
  const [featureStepIndex, setFeatureStepIndex] = useState(0)
  const [activeTourTarget, setActiveTourTarget] = useState<string>()
  const [activatedTourTarget, setActivatedTourTarget] = useState<string>()
  const autoStartedKeys = useRef(new Set<string>())
  const ready = identity !== null && loadedKey === storageKey

  useEffect(() => {
    if (identity === null) return
    let loadedPreferences = loadPreferences(storageKey)
    if (identity !== 'guest') {
      try {
        const resumedPreferences = resumeGuideAfterAccountHandoff(
          loadedPreferences,
          window.sessionStorage.getItem(GUIDE_ACCOUNT_HANDOFF_KEY)
        )
        if (resumedPreferences !== loadedPreferences) {
          loadedPreferences = resumedPreferences
          window.localStorage.setItem(storageKey, JSON.stringify(loadedPreferences))
        }
        window.sessionStorage.removeItem(GUIDE_ACCOUNT_HANDOFF_KEY)
      } catch {
        // Storage failures must not block account loading or the in-memory guide.
      }
    }
    preferencesRef.current = loadedPreferences
    setPreferences(loadedPreferences)
    setLoadedKey(storageKey)
    setPendingFeature(null)
    setFeatureStepIndex(0)
  }, [identity, storageKey])

  useEffect(() => {
    if (!ready || identity !== 'guest') return
    try {
      if (
        preferences.interactiveEnabled &&
        !preferences.tourPaused &&
        (preferences.activeTourStep === 'account' || preferences.activeTourStep === 'team')
      ) {
        window.sessionStorage.setItem(GUIDE_ACCOUNT_HANDOFF_KEY, createGuideAccountHandoff())
      } else {
        window.sessionStorage.removeItem(GUIDE_ACCOUNT_HANDOFF_KEY)
      }
    } catch {
      // The guide remains usable in-memory when session storage is unavailable.
    }
  }, [identity, preferences.activeTourStep, preferences.interactiveEnabled, preferences.tourPaused, ready])

  const updatePreferences = useCallback(
    (update: (current: GuidePreferences) => GuidePreferences) => {
      if (!ready) return
      const next = persistGuidePreferenceUpdate(preferencesRef.current, update, (serialized) => {
        window.localStorage.setItem(storageKey, serialized)
      })
      preferencesRef.current = next
      setPreferences(next)
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
        setFeatureStepIndex(0)
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
    setFeatureStepIndex(0)
  }, [updatePreferences])

  const completeTeamSetup = useCallback(() => {
    if (preferencesRef.current.activeTourStep !== 'team') return
    updatePreferences(completeTeamGuide)
  }, [updatePreferences])

  const introduceFeature = useCallback(
    (feature: GuideFeature, context: GuideFeatureContext = {}) => {
      if (!ready || !preferences.interactiveEnabled || preferences.seenFeatures.includes(feature)) return
      setPendingFeature((current) => {
        if (!current) return { feature, context }
        if (current.feature !== feature) return current
        if (
          current.context.eventVpnRequired === context.eventVpnRequired &&
          current.context.hasAttachment === context.hasAttachment &&
          current.context.instanceActive === context.instanceActive
        ) {
          return current
        }
        return { feature, context }
      })
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
  const tourTarget = useCallback(
    (step: GuideTourStep) =>
      guideTourTargetSelector({
        step,
        pathname: location.pathname,
        signedIn: Boolean(user),
        preferOAuth: config.allowPasswordRegistration === false,
        challengeFeature: pendingFeature?.feature,
        instanceActive: pendingFeature?.context.instanceActive,
      }),
    [
      config.allowPasswordRegistration,
      location.pathname,
      pendingFeature?.context.instanceActive,
      pendingFeature?.feature,
      user,
    ]
  )

  const steps = useMemo<TourStep[]>(
    () => [
      {
        id: 'welcome',
        title: t('guide.tour.welcome.title', 'Learn the playground'),
        body: t(
          'guide.tour.welcome.body',
          'Follow the cursor and select each highlighted control. On mobile, open More first.'
        ),
        note: t('guide.tour.welcome.note', 'The guide continues when you change pages.'),
        targetSelector: tourTarget('welcome'),
        requiresTargetActivation: true,
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
        targetSelector: tourTarget('account'),
        requiresTargetActivation: !user && location.pathname.startsWith('/account/'),
        targetPrompt: !user
          ? t(
              'guide.tour.account.action',
              'Start with the highlighted sign-in option. You can use the whole form; the guide resumes after sign-in.'
            )
          : undefined,
      },
      {
        id: 'team',
        title: t('guide.tour.team.title', 'Create or join a team'),
        body: user
          ? isTeamPage
            ? t(
                'guide.tour.team.destination_body',
                'Choose Create or Join, then type in the highlighted field. The cursor moves to the button when it is ready.'
              )
            : t('guide.tour.team.body', 'Open Teams, then create a team or join one with an invite code.')
          : t('guide.tour.team.guest_body', 'Sign in first. Events are entered with a team.'),
        note: isTeamPage
          ? t('guide.tour.team.form_note', 'The guide waits here until the platform confirms that your team is ready.')
          : t('guide.tour.team.note', 'One person creates the team; everyone else joins with its invite code.'),
        path: user && !isTeamPage ? '/teams' : !user ? '/account/login' : undefined,
        pathLabel: user ? t('guide.tour.team.open', 'Open teams') : t('guide.tour.team.login_first', 'Sign in first'),
        targetSelector: tourTarget('team'),
        requiresTargetActivation: true,
        targetPrompt:
          user && isTeamPage
            ? t(
                'guide.tour.team.form_action',
                'Type in the highlighted field. When the cursor moves to Create or Join, select it.'
              )
            : undefined,
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
        targetSelector: tourTarget('events'),
        requiresTargetActivation: location.pathname === '/games',
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
        targetSelector: tourTarget('challenges'),
        requiresTargetActivation: isChallengePage,
      },
      {
        id: 'connection',
        title:
          pendingFeature?.feature === 'static-challenge'
            ? t('guide.tour.connection.static_title', 'Static challenge: no connection needed')
            : t('guide.tour.connection.title', 'Start and connect'),
        body:
          pendingFeature?.feature === 'static-challenge'
            ? t(
                'guide.tour.connection.static_body',
                'This challenge has no service to start. Read its material, then select the highlighted flag field.'
              )
            : connectionBody,
        note: t('guide.tour.connection.note', 'VPN-only events use their event VPN instead of the platform proxy.'),
        targetSelector: tourTarget('connection'),
        requiresTargetActivation: true,
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
        targetSelector: tourTarget('submit'),
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
      pendingFeature?.context.instanceActive,
      pendingFeature?.feature,
      t,
      tourTarget,
      user,
    ]
  )

  const featureSteps = useMemo<FeatureStep[]>(() => {
    if (!pendingFeature) return []

    const feature = pendingFeature.feature
    if (feature === 'event-vpn') {
      return [
        {
          id: 'vpn-profile',
          title: t('guide.feature.vpn.title', 'This event requires its VPN'),
          body: t(
            'guide.feature.vpn.body',
            'Download the event profile, import it into WireGuard, and connect before opening any private challenge address.'
          ),
          note: t('guide.feature.vpn.note', 'Keep the profile private. It identifies your team’s event access.'),
          targetSelector: '[data-guide="event-vpn-download"]',
        },
      ]
    }

    if (feature === 'static-challenge') {
      const staticSteps: FeatureStep[] = [
        {
          id: 'material',
          title: t('guide.feature.static.material_title', 'Read the challenge material'),
          body: t(
            'guide.feature.static.material_body',
            'This is a static challenge, so there is no instance to start. Read the description and hints first.'
          ),
          note: t(
            pendingFeature.context.eventVpnRequired
              ? 'guide.feature.static.material_vpn_note'
              : 'guide.feature.static.material_note',
            pendingFeature.context.eventVpnRequired
              ? 'The event VPN still controls access to this page, but this challenge has no service instance.'
              : 'The challenge may be solved entirely from the text, an attachment, or both.'
          ),
          targetSelector: '[data-guide="challenge-material"]',
          advanceOnActivate: true,
        },
      ]
      if (pendingFeature.context.hasAttachment) {
        staticSteps.push({
          id: 'attachment',
          title: t('guide.feature.static.attachment_title', 'Download and verify the attachment'),
          body: t(
            'guide.feature.static.attachment_body',
            'Use the highlighted attachment control. The filename, size, and SHA-256 help you verify the real challenge file.'
          ),
          note: t(
            'guide.feature.static.attachment_note',
            'Keep the original file unchanged and do your analysis on a copy when practical.'
          ),
          targetSelector: '[data-guide="challenge-attachment-download"]',
          advanceOnActivate: true,
        })
      }
      staticSteps.push({
        id: 'static-submit',
        title: t('guide.feature.static.submit_title', 'Submit the exact flag'),
        body: t(
          'guide.feature.static.submit_body',
          'When you find the flag, paste only the flag into the highlighted field and wait for the verdict.'
        ),
        note: t(
          pendingFeature.context.eventVpnRequired
            ? 'guide.feature.static.submit_vpn_note'
            : 'guide.feature.static.submit_note',
          pendingFeature.context.eventVpnRequired
            ? 'Keep the event VPN connected, but do not look for a WSRX tunnel or challenge port.'
            : 'Static challenges do not need WSRX, a challenge port, or an event VPN.'
        ),
        targetSelector: '[data-guide="flag-submit"]',
      })
      return staticSteps
    }

    const startStep: FeatureStep = {
      id: 'start',
      title:
        feature === 'container-vpn'
          ? t('guide.feature.container.vpn_start_title', 'Connect the VPN, then start the instance')
          : t('guide.feature.container.start_title', 'Start your challenge instance'),
      body:
        feature === 'container-vpn'
          ? t(
              'guide.feature.container.vpn_start_body',
              'Make sure the event WireGuard profile is connected, then select Start instance. Wait for the success message.'
            )
          : t(
              'guide.feature.container.start_body',
              'Select Start instance. The first start may build or pull an image on demand, so wait instead of clicking repeatedly.'
            ),
      note: t(
        'guide.feature.container.start_note',
        'This guide continues automatically only after the instance starts successfully.'
      ),
      targetSelector: '[data-guide="instance-start"]',
    }

    if (feature === 'container-wsrx') {
      return [
        startStep,
        {
          id: 'wsrx-setup',
          title: t('guide.feature.wsrx.setup_title', 'Run WSRX on your computer'),
          body: t(
            'guide.feature.wsrx.setup_body',
            'Keep Local WSRX selected. Download and start WebSocketReflectorX, then approve the browser connection if your computer asks.'
          ),
          note: t(
            'guide.feature.wsrx.setup_note',
            'Connection tools in the navigation bar shows whether the local WSRX app is connected. The platform retries automatically after it starts.'
          ),
          targetSelector: '[data-guide="wsrx-download"]',
          advanceOnActivate: true,
        },
        {
          id: 'wsrx-copy',
          title: t('guide.feature.wsrx.copy_title', 'Wait for the local tunnel, then copy it'),
          body: t(
            'guide.feature.wsrx.copy_body',
            'Wait until the status says the tunnel is ready and the field contains a 127.0.0.1 address, then use the highlighted Copy button.'
          ),
          note: t(
            'guide.feature.wsrx.copy_note',
            'The WSS URL is not a netcat address. For nc, keep Local WSRX selected and use the 127.0.0.1 address.'
          ),
          targetSelector: '[data-guide="instance-copy"][data-entry-mode="wsrx"]',
          advanceOnActivate: true,
        },
        {
          id: 'wsrx-connect',
          title: t('guide.feature.wsrx.connect_title', 'Connect through the local WSRX address'),
          body: t(
            'guide.feature.wsrx.connect_body',
            'Split the copied local address into its host and port, then use the protocol named by the challenge. For a TCP challenge, run:'
          ),
          command: 'nc 127.0.0.1 <port>',
          note: t(
            'guide.feature.wsrx.connect_note',
            'Keep WebSocketReflectorX running while you play. Switch to WSS only when your client understands WebSockets and needs the raw wss:// URL.'
          ),
          targetSelector: '[data-guide="instance-entry"]',
        },
      ]
    }

    if (feature === 'container-vpn') {
      return [
        startStep,
        {
          id: 'vpn-copy',
          title: t('guide.feature.container.vpn_copy_title', 'Copy the private host and port'),
          body: t(
            'guide.feature.container.vpn_copy_body',
            'After the instance is ready, copy the displayed private host and port. It is reachable only through the event VPN.'
          ),
          note: t(
            'guide.feature.container.vpn_copy_note',
            'Do not replace this address with the platform proxy or share it outside your team.'
          ),
          targetSelector: '[data-guide="instance-copy"]',
          advanceOnActivate: true,
        },
        {
          id: 'vpn-connect',
          title: t('guide.feature.container.vpn_connect_title', 'Use the challenge protocol over VPN'),
          body: t(
            'guide.feature.container.vpn_connect_body',
            'Use the copied host and port with the protocol in the challenge description. A TCP service usually uses nc; a web service uses a browser.'
          ),
          command: 'nc <private-host> <port>',
          note: t('guide.feature.container.vpn_connect_note', 'Leave WireGuard connected while using the instance.'),
          targetSelector: '[data-guide="instance-entry"]',
        },
      ]
    }

    if (feature === 'container-direct') {
      return [
        startStep,
        {
          id: 'direct-copy',
          title: t('guide.feature.container.direct_copy_title', 'Copy the public host and port'),
          body: t(
            'guide.feature.container.direct_copy_body',
            'After the instance is ready, use the highlighted Copy button to copy its direct host-and-port address.'
          ),
          note: t(
            'guide.feature.container.direct_copy_note',
            'This mode does not need WSRX. An event VPN can still override it when the event requires one.'
          ),
          targetSelector: '[data-guide="instance-copy"]',
          advanceOnActivate: true,
        },
        {
          id: 'direct-connect',
          title: t('guide.feature.container.direct_connect_title', 'Use the challenge protocol'),
          body: t(
            'guide.feature.container.direct_connect_body',
            'Use the copied address with the protocol in the challenge description. For a TCP service, split the host and port and run:'
          ),
          command: 'nc <host> <port>',
          note: t(
            'guide.feature.container.direct_connect_note',
            'For an HTTP service, open the displayed address in a browser instead.'
          ),
          targetSelector: '[data-guide="instance-entry"]',
        },
      ]
    }

    return []
  }, [pendingFeature, t])

  const activeStepIndex = preferences.activeTourStep ? GUIDE_TOUR_STEPS.indexOf(preferences.activeTourStep) : -1
  const stepIndex = activeStepIndex >= 0 ? activeStepIndex : 0
  const step = steps[stepIndex]
  const tourOpen = ready && preferences.activeTourStep !== null && !preferences.tourPaused
  const destinationPath = step.path?.split(/[?#]/, 1)[0]
  const atStepDestination = Boolean(destinationPath && location.pathname === destinationPath)
  const needsNavigation = Boolean(step.path && !atStepDestination)
  const needsTargetActivation = Boolean(step.requiresTargetActivation && !needsNavigation)
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

  const moveToNextStep = useCallback(() => {
    const next = steps[Math.min(steps.length - 1, stepIndex + 1)]
    updatePreferences((current) => setGuideTourStep(current, next.id))
    if (next.path) navigate(next.path)
  }, [navigate, stepIndex, steps, updatePreferences])

  const onTourTargetActivate = useCallback(
    (target: string | undefined) => {
      setActivatedTourTarget(target)
      const nextStep = nextGuideStepForTarget(step.id, target)
      if (nextStep) updatePreferences((current) => setGuideTourStep(current, nextStep))
    },
    [step.id, updatePreferences]
  )

  useEffect(() => {
    setActivatedTourTarget(undefined)
  }, [location.pathname, step.id])

  const teamGuideAction = resolveTeamGuideAction(activeTourTarget, activatedTourTarget)
  const teamGuideNeedsKeyboard = teamGuideAction === 'type-create-name' || teamGuideAction === 'paste-join-code'
  const teamGuideKeyboardActive = step.id === 'team' && Boolean(user) && isTeamPage && teamGuideNeedsKeyboard
  const teamGuidePrompt =
    teamGuideAction === 'select-create-name'
      ? t('guide.tour.team.select_create_name', 'Select the highlighted Team name field.')
      : teamGuideAction === 'type-create-name'
        ? t('guide.tour.team.type_create_name', 'Good—now type your team name. The cursor moves when it is ready.')
        : teamGuideAction === 'select-join-code'
          ? t('guide.tour.team.select_join_code', 'Select the highlighted Invite code field.')
          : teamGuideAction === 'paste-join-code'
            ? t('guide.tour.team.paste_join_code', 'Good—now paste the invite code your teammate sent you.')
            : teamGuideAction === 'submit-create'
              ? t('guide.tour.team.submit_create', 'Your team name is ready. Select Create Team.')
              : teamGuideAction === 'submit-join'
                ? t('guide.tour.team.submit_join', 'Your invite code is ready. Select Join.')
                : t('guide.tour.team.choose_action', 'Select Create or Join to begin.')

  useEffect(() => {
    setFeatureStepIndex(0)
  }, [pendingFeature?.feature])

  useEffect(() => {
    if (!pendingFeature?.feature.startsWith('container-') || !pendingFeature.context.instanceActive) return
    setFeatureStepIndex((current) => (current === 0 ? 1 : current))
  }, [pendingFeature?.context.instanceActive, pendingFeature?.feature])

  const boundedFeatureStepIndex = Math.min(featureStepIndex, Math.max(featureSteps.length - 1, 0))
  const featureStep = featureSteps[boundedFeatureStepIndex]
  const featureRequiresAction = Boolean(
    featureStep &&
    boundedFeatureStepIndex < featureSteps.length - 1 &&
    (featureStep.advanceOnActivate || (featureStep.id === 'start' && !pendingFeature?.context.instanceActive))
  )
  const moveFeatureStep = useCallback(
    (index: number) => {
      setFeatureStepIndex(Math.min(featureSteps.length - 1, Math.max(0, index)))
    },
    [featureSteps.length]
  )

  const onFeatureTargetActivate = useCallback(() => {
    if (!featureStep?.advanceOnActivate || boundedFeatureStepIndex >= featureSteps.length - 1) return
    setFeatureStepIndex((current) => Math.min(featureSteps.length - 1, current + 1))
  }, [boundedFeatureStepIndex, featureStep?.advanceOnActivate, featureSteps.length])

  const dismissFeature = () => {
    if (pendingFeature) updatePreferences((current) => markGuideFeatureSeen(current, pendingFeature.feature))
    setPendingFeature(null)
    setFeatureStepIndex(0)
  }

  const value = useMemo<PlayerGuideContextValue>(
    () => ({
      preferences,
      ready,
      startGuide,
      setInteractiveEnabled,
      resetGuide,
      completeTeamSetup,
      introduceFeature,
    }),
    [completeTeamSetup, introduceFeature, preferences, ready, resetGuide, setInteractiveEnabled, startGuide]
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
        onTargetChange={setActiveTourTarget}
        showTargetCursor={!teamGuideKeyboardActive}
        progress={{
          current: stepIndex + 1,
          total: steps.length,
          label: t('guide.tour.progress', 'Step {{current}} of {{total}}', {
            current: stepIndex + 1,
            total: steps.length,
          }),
        }}
      >
        <Stack gap="xs" className={classes.tourBody}>
          <Stack
            gap="sm"
            className={classes.tourContent}
            role="region"
            tabIndex={0}
            aria-label={t('guide.tour.instructions', 'Guide instructions')}
          >
            <Stack gap="xs" role="status" aria-live="polite" aria-atomic="true">
              <Text size="sm">{step.body}</Text>
              <Text size="sm" c="dimmed" className={classes.note}>
                {step.note}
              </Text>
            </Stack>
            {needsNavigation && (
              <Button
                variant="light"
                leftSection={<Icon path={mdiOpenInNew} size={0.72} aria-hidden="true" />}
                onClick={() => navigate(step.path!)}
                className={classes.guideAction}
              >
                {step.pathLabel}
              </Button>
            )}
          </Stack>
          <GuideTargetPrompt keyboardEntry={teamGuideKeyboardActive}>
            {step.id === 'team' && user && isTeamPage && !needsNavigation
              ? teamGuidePrompt
              : step.targetPrompt && !needsNavigation
                ? step.targetPrompt
                : needsNavigation
                  ? t('guide.tour.open_destination', 'Open the page above. This step continues there.')
                  : needsTargetActivation
                    ? t('guide.tour.destination_ready', 'Select the highlighted control to continue.')
                    : t('guide.tour.target_optional', 'Use the highlighted control, or choose Next.')}
          </GuideTargetPrompt>
          <Group justify="space-between" gap="xs" wrap="nowrap" className={classes.tourFooter}>
            <Button
              variant="subtle"
              color="gray"
              aria-label={t('guide.tour.disable', 'Stop guide')}
              onClick={() => setInteractiveEnabled(false)}
            >
              {t('guide.tour.stop_short', 'Stop')}
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
              ) : !needsNavigation && !needsTargetActivation ? (
                <Button
                  rightSection={<Icon path={mdiArrowRight} size={0.7} aria-hidden="true" />}
                  onClick={moveToNextStep}
                >
                  {t('common.pagination.next', 'Next')}
                </Button>
              ) : null}
            </Group>
          </Group>
        </Stack>
      </AccessibleGuideModal>

      <AccessibleGuideModal
        opened={Boolean(pendingFeature && featureStep) && !tourOpen && ready}
        onClose={dismissFeature}
        title={featureStep?.title ?? t('guide.feature.title', 'Challenge guide')}
        size="min(21rem, calc(100vw - 1rem))"
        closeLabel={t('guide.feature.dismiss', 'Dismiss this tip')}
        overlayOpacity={0.58}
        targetSelector={featureStep?.targetSelector}
        onTargetActivate={onFeatureTargetActivate}
        progress={
          featureStep
            ? {
                current: boundedFeatureStepIndex + 1,
                total: featureSteps.length,
                label: t('guide.feature.progress', 'Step {{current}} of {{total}}', {
                  current: boundedFeatureStepIndex + 1,
                  total: featureSteps.length,
                }),
              }
            : undefined
        }
      >
        {pendingFeature && featureStep && (
          <Stack gap="xs" className={classes.tourBody}>
            <Stack
              gap="sm"
              className={classes.tourContent}
              role="region"
              tabIndex={0}
              aria-label={t('guide.feature.instructions', 'Challenge guide instructions')}
            >
              <Stack gap="xs" role="status" aria-live="polite" aria-atomic="true">
                <Text size="sm">{featureStep.body}</Text>
                {featureStep.command && (
                  <Text component="code" size="sm" className={classes.command}>
                    {featureStep.command}
                  </Text>
                )}
                {featureStep.note && (
                  <Text size="sm" c="dimmed" className={classes.note}>
                    {featureStep.note}
                  </Text>
                )}
              </Stack>
              <Button
                variant="default"
                onClick={() => {
                  dismissFeature()
                  navigate('/guide#play-challenge')
                }}
              >
                {t('guide.feature.full_guide', 'Open the full guide')}
              </Button>
            </Stack>
            <GuideTargetPrompt>
              {featureRequiresAction
                ? t('guide.feature.use_highlight', 'Complete the highlighted action to continue.')
                : t('guide.feature.try_highlight', 'Use the highlighted control while following this step.')}
            </GuideTargetPrompt>
            <Group justify="space-between" gap="xs" wrap="nowrap" className={classes.tourFooter}>
              <Button
                variant="subtle"
                color="gray"
                aria-label={t('guide.feature.disable', 'Stop tips')}
                onClick={() => setInteractiveEnabled(false)}
              >
                {t('guide.feature.stop_short', 'Stop')}
              </Button>
              <Group gap={4} wrap="nowrap">
                <Button
                  variant="default"
                  disabled={boundedFeatureStepIndex === 0}
                  leftSection={<Icon path={mdiArrowLeft} size={0.7} aria-hidden="true" />}
                  onClick={() => moveFeatureStep(boundedFeatureStepIndex - 1)}
                >
                  {t('common.pagination.previous', 'Previous')}
                </Button>
                {boundedFeatureStepIndex === featureSteps.length - 1 ? (
                  <Button leftSection={<Icon path={mdiCheck} size={0.7} aria-hidden="true" />} onClick={dismissFeature}>
                    {t('guide.feature.understood', 'Got it')}
                  </Button>
                ) : !featureRequiresAction ? (
                  <Button
                    rightSection={<Icon path={mdiArrowRight} size={0.7} aria-hidden="true" />}
                    onClick={() => moveFeatureStep(boundedFeatureStepIndex + 1)}
                  >
                    {t('common.pagination.next', 'Next')}
                  </Button>
                ) : null}
              </Group>
            </Group>
          </Stack>
        )}
      </AccessibleGuideModal>
    </PlayerGuideContext.Provider>
  )
}
