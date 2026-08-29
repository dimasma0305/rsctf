import { Alert, Anchor, Badge, Button, Card, Group, List, Stack, Switch, Text, ThemeIcon, Title } from '@mantine/core'
import {
  mdiAccountCircleOutline,
  mdiBookOpenPageVariantOutline,
  mdiCheckCircleOutline,
  mdiCursorDefaultClickOutline,
  mdiFlagOutline,
  mdiGamepadVariantOutline,
  mdiInformationOutline,
  mdiLanConnect,
  mdiPlayCircleOutline,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { Link } from 'react-router'
import { PageHeader } from '@Components/PageHeader'
import { WithNavBar } from '@Components/WithNavbar'
import { usePlayerGuide } from '@Components/guide/PlayerGuide'
import { useConfig } from '@Hooks/useConfig'
import { usePageTitle } from '@Hooks/usePageTitle'
import { useUser } from '@Hooks/useUser'
import { ContainerPortMappingType } from '@Api'
import classes from '@Styles/PlayerGuidePage.module.css'

interface GuideSectionProps {
  id: string
  number: number
  icon: string
  title: string
  summary: string
  children: ReactNode
  image?: string
  imageAlt?: string
}

const GuideSection: FC<GuideSectionProps> = ({ id, number, icon, title, summary, children, image, imageAlt }) => (
  <section id={id} className={`${classes.section} ${image ? '' : classes.sectionFull}`} aria-labelledby={`${id}-title`}>
    <div className={classes.sectionCopy}>
      <Group gap="sm" align="center" wrap="nowrap">
        <ThemeIcon size={44} radius="xl" variant="light" aria-hidden="true">
          <Icon path={icon} size={0.95} />
        </ThemeIcon>
        <Stack gap={0}>
          <Text size="xs" fw={700} tt="uppercase" c="dimmed">
            Step {number}
          </Text>
          <Title order={2} id={`${id}-title`} size="h3">
            {title}
          </Title>
        </Stack>
      </Group>
      <Text size="lg" className={classes.summary}>
        {summary}
      </Text>
      {children}
    </div>
    {image && (
      <figure className={classes.screenshot}>
        <div className={classes.browserBar} aria-hidden="true">
          <span />
          <span />
          <span />
        </div>
        <img src={image} alt={imageAlt ?? ''} loading="lazy" decoding="async" />
      </figure>
    )}
  </section>
)

interface InstructionStep {
  title: string
  description: string
  image: string
  imageAlt: string
}

const InstructionGallery: FC<{ label: string; steps: InstructionStep[] }> = ({ label, steps }) => (
  <ol className={classes.instructionGrid} aria-label={label}>
    {steps.map((step, index) => (
      <li key={step.image} className={classes.instructionStep}>
        <figure className={classes.instructionFigure}>
          <div className={classes.browserBar} aria-hidden="true">
            <span />
            <span />
            <span />
          </div>
          <img src={step.image} alt={step.imageAlt} loading="lazy" decoding="async" />
          <figcaption className={classes.instructionCaption}>
            <Icon
              path={mdiCursorDefaultClickOutline}
              size={1.25}
              className={classes.instructionCursor}
              aria-hidden="true"
            />
            <div>
              <Text size="xs" fw={700} tt="uppercase" c="dimmed">
                Step {index + 1}
              </Text>
              <Text fw={700}>{step.title}</Text>
              <Text size="sm" c="dimmed">
                {step.description}
              </Text>
            </div>
          </figcaption>
        </figure>
      </li>
    ))}
  </ol>
)

const Guide: FC = () => {
  const { t } = useTranslation()
  const { config } = useConfig()
  const { user } = useUser()
  const guide = usePlayerGuide()
  usePageTitle(t('common.tab.guide', 'Guide'))

  const providers = [config.enableGoogleAuth ? 'Google' : null, config.enableDiscordAuth ? 'Discord' : null].filter(
    (provider): provider is string => Boolean(provider)
  )
  const registrationDescription =
    config.allowRegister === false
      ? t(
          'guide.page.account.registration_closed',
          'Registration is currently closed. Use an existing account or contact the organizer.'
        )
      : config.allowPasswordRegistration === false
        ? t('guide.page.account.oauth_only', 'Registration is OAuth-only through {{providers}}.', {
            providers: providers.join(' or ') || t('guide.page.account.configured_provider', 'the configured provider'),
          })
        : t('guide.page.account.password_available', 'Register with email and password{{oauth}}.', {
            oauth: providers.length
              ? t('guide.page.account.oauth_available', ', or continue with {{providers}}', {
                  providers: providers.join(' / '),
                })
              : '',
          })
  const connectionDescription =
    config.portMapping === ContainerPortMappingType.PlatformProxy
      ? t(
          'guide.page.connection.proxy',
          'This platform uses Platform Proxy by default. Its connection tool maps the private challenge endpoint to a local address; copy the local address shown in the instance panel.'
        )
      : t(
          'guide.page.connection.direct',
          'This platform uses direct host-and-port connections by default. Copy the address shown in the instance panel.'
        )

  const quickLinks = [
    ['account-access', t('guide.page.contents.account', 'Account')],
    ['find-event', t('guide.page.contents.event', 'Find an event')],
    ['join-event', t('guide.page.contents.join', 'Join')],
    ['play-challenge', t('guide.page.contents.play', 'Play')],
    ['submit-flag', t('guide.page.contents.submit', 'Submit')],
  ]
  const joinSteps: InstructionStep[] = [
    {
      title: t('guide.page.join.open_title', 'Open the event briefing'),
      description: t(
        'guide.page.join.open_description',
        'Check the schedule, format, eligibility, VPN requirement, and team rules before joining.'
      ),
      image: '/static/guide/join-event.webp',
      imageAlt: t(
        'guide.page.join.open_image_alt',
        'Event briefing page with the Join event button highlighted below the schedule and participation rules.'
      ),
    },
    {
      title: t('guide.page.join.confirm_title', 'Confirm that you understand the rules'),
      description: t(
        'guide.page.join.confirm_description',
        'Read the participation warning, then confirm only when you are ready to represent your team.'
      ),
      image: '/static/guide/join-confirm.webp',
      imageAlt: t(
        'guide.page.join.confirm_image_alt',
        'Join confirmation dialog showing the event participation warning and the highlighted Confirm button.'
      ),
    },
    {
      title: t('guide.page.join.choose_title', 'Choose the participating team'),
      description: t(
        'guide.page.join.choose_description',
        'Select your team. Division and invite-code fields appear only when that event requires them.'
      ),
      image: '/static/guide/join-team.webp',
      imageAlt: t(
        'guide.page.join.choose_image_alt',
        'Join event dialog with the team selector and Join button highlighted in sequence.'
      ),
    },
    {
      title: t('guide.page.join.status_title', 'Verify the participation status'),
      description: t(
        'guide.page.join.status',
        'Pending means an organizer must accept the request; Approved or Joined means your team can enter when the event opens.'
      ),
      image: '/static/guide/join-status.webp',
      imageAlt: t(
        'guide.page.join.status_image_alt',
        'Event page after a request showing a clearly labelled Pending, Approved, or Joined participation status.'
      ),
    },
  ]

  return (
    <WithNavBar withFooter withHeader stickyHeader>
      <PageHeader
        eyebrow={t('guide.page.eyebrow', 'Player handbook')}
        title={t('guide.page.title', 'How to play')}
        description={t(
          'guide.page.description',
          'A platform-aware walkthrough for signing in, joining an event, opening challenges, connecting to instances, and submitting flags.'
        )}
        actions={
          <Button
            leftSection={<Icon path={mdiPlayCircleOutline} size={0.8} aria-hidden="true" />}
            onClick={guide.startGuide}
            disabled={!guide.ready}
          >
            {t('guide.page.start_interactive', 'Start interactive guide')}
          </Button>
        }
      />

      <Stack gap="xl" className={classes.page}>
        <Card withBorder className={classes.guideControl}>
          <Group justify="space-between" align="center" gap="lg" wrap="wrap">
            <Stack gap={3}>
              <Group gap="xs">
                <Icon path={mdiGamepadVariantOutline} size={0.85} aria-hidden="true" />
                <Text fw={700}>{t('guide.page.interactive_title', 'Interactive tips')}</Text>
              </Group>
              <Text size="sm" c="dimmed">
                {t(
                  'guide.page.interactive_description',
                  'Show the first-run walkthrough and one-time explanations when you encounter features such as container instances or an event VPN.'
                )}
              </Text>
            </Stack>
            <Group gap="sm">
              <Switch
                checked={guide.preferences.interactiveEnabled}
                disabled={!guide.ready}
                label={
                  guide.preferences.interactiveEnabled
                    ? t('guide.page.tips_on', 'Tips on')
                    : t('guide.page.tips_off', 'Tips off')
                }
                onChange={(event) => guide.setInteractiveEnabled(event.currentTarget.checked)}
              />
              <Button variant="default" onClick={guide.resetGuide} disabled={!guide.ready}>
                {t('guide.page.reset', 'Restart from the beginning')}
              </Button>
            </Group>
          </Group>
        </Card>

        <nav className={classes.contents} aria-label={t('guide.page.contents.title', 'Guide sections')}>
          {quickLinks.map(([id, label], index) => (
            <Anchor key={id} href={`#${id}`} className={classes.contentsLink}>
              <Icon path={mdiCursorDefaultClickOutline} size={0.68} aria-hidden="true" />
              <span className="app-sr-only">Step {index + 1}: </span>
              {label}
            </Anchor>
          ))}
        </nav>

        <GuideSection
          id="account-access"
          number={1}
          icon={mdiAccountCircleOutline}
          title={t('guide.page.account.title', 'Register or sign in')}
          summary={registrationDescription}
          image="/static/guide/login.webp"
          imageAlt={t(
            'guide.page.account.image_alt',
            'RSCTF login page showing the account form and available sign-in options.'
          )}
        >
          <List spacing="xs" className={classes.list}>
            <List.Item>
              {t('guide.page.account.one_account', 'Use one account for your teams and event history.')}
            </List.Item>
            {config.emailConfirmationRequired && (
              <List.Item>
                {t('guide.page.account.confirm_email', 'Open the confirmation email before trying to join an event.')}
              </List.Item>
            )}
            <List.Item>
              {t('guide.page.account.team', 'After sign-in, create a team or accept a team invitation.')}
            </List.Item>
          </List>
          <Group gap="sm">
            <Button component={Link} to={user ? '/account/profile' : '/account/login'} variant="light">
              {user ? t('guide.page.account.account', 'Account page') : t('common.tab.account.login', 'Login')}
            </Button>
            {!user && config.allowRegister !== false && (
              <Button component={Link} to="/account/register" variant="default">
                {t('guide.page.account.register', 'Register')}
              </Button>
            )}
          </Group>
        </GuideSection>

        <GuideSection
          id="find-event"
          number={2}
          icon={mdiBookOpenPageVariantOutline}
          title={t('guide.page.event.title', 'Find the right event')}
          summary={t(
            'guide.page.event.summary',
            'Open Games, use the compact search, and check each event’s start time, status, and participation badge.'
          )}
          image="/static/guide/games.webp"
          imageAlt={t(
            'guide.page.event.image_alt',
            'Games catalog showing compact search, participation filters, and joined event indicators.'
          )}
        >
          <List spacing="xs" className={classes.list}>
            <List.Item>{t('guide.page.event.search', 'Search by event title, summary, or exact event ID.')}</List.Item>
            <List.Item>
              {t('guide.page.event.filter', 'When signed in, filter All, Joined, or Not joined events.')}
            </List.Item>
            <List.Item>
              {t('guide.page.event.time', 'Check the displayed time in your selected locale before joining.')}
            </List.Item>
          </List>
          <Button component={Link} to="/games" variant="light">
            {t('guide.page.event.open', 'Browse games')}
          </Button>
        </GuideSection>

        <GuideSection
          id="join-event"
          number={3}
          icon={mdiCheckCircleOutline}
          title={t('guide.page.join.title', 'Join with your team')}
          summary={t(
            'guide.page.join.summary',
            'Open the event, review its rules, select your team, and submit the join request.'
          )}
        >
          <InstructionGallery
            label={t('guide.page.join.steps_label', 'Step-by-step images for joining an event with your team')}
            steps={joinSteps}
          />
        </GuideSection>

        <GuideSection
          id="play-challenge"
          number={4}
          icon={mdiLanConnect}
          title={t('guide.page.play.title', 'Open and run a challenge')}
          summary={connectionDescription}
          image="/static/guide/challenge.webp"
          imageAlt={t(
            'guide.page.play.image_alt',
            'Challenge workspace showing challenge cards, the instance start control, and flag submission area.'
          )}
        >
          <List spacing="xs" className={classes.list}>
            <List.Item>
              {t(
                'guide.page.play.catalog',
                'Use My challenges to search only challenges from accepted, started events you joined.'
              )}
            </List.Item>
            <List.Item>
              {t(
                'guide.page.play.start',
                'For a container challenge, select Start instance. An on-demand image build can make the first start slower; wait for the success message instead of starting repeatedly.'
              )}
            </List.Item>
            <List.Item>
              {t(
                'guide.page.play.vpn',
                'If the event says VPN required, download its WireGuard profile first. VPN-only events use their event port instructions instead of the platform proxy default.'
              )}
            </List.Item>
            <List.Item>
              {t(
                'guide.page.play.cleanup',
                'Destroy an unused instance so the event can reclaim CPU, memory, and storage.'
              )}
            </List.Item>
          </List>
          <Stack gap="sm" aria-label={t('guide.page.play.modes_label', 'Challenge connection modes')}>
            <Card withBorder padding="md">
              <Group justify="space-between" gap="sm" wrap="wrap">
                <Text fw={700}>{t('guide.page.play.static_title', 'Static challenge')}</Text>
                <Badge variant="light" color="gray">
                  {t('guide.page.play.no_instance', 'No instance')}
                </Badge>
              </Group>
              <Text size="sm" c="dimmed" mt="xs">
                {t(
                  'guide.page.play.static_body',
                  'Read the description, download and verify any attachment, solve locally, then submit the flag. No WSRX, port, or VPN is expected.'
                )}
              </Text>
            </Card>
            <Card withBorder padding="md">
              <Group justify="space-between" gap="sm" wrap="wrap">
                <Text fw={700}>{t('guide.page.play.direct_title', 'Direct host and port')}</Text>
                {config.portMapping !== ContainerPortMappingType.PlatformProxy && (
                  <Badge variant="light">{t('guide.page.play.platform_default', 'Platform default')}</Badge>
                )}
              </Group>
              <List type="ordered" spacing={4} size="sm" mt="xs">
                <List.Item>
                  {t('guide.page.play.direct_start', 'Start the instance and wait until it is ready.')}
                </List.Item>
                <List.Item>{t('guide.page.play.direct_copy', 'Copy the displayed host and port.')}</List.Item>
                <List.Item>
                  {t(
                    'guide.page.play.direct_use',
                    'Use the protocol named by the challenge, such as nc <host> <port> for TCP or a browser for HTTP.'
                  )}
                </List.Item>
              </List>
            </Card>
            <Card withBorder padding="md">
              <Group justify="space-between" gap="sm" wrap="wrap">
                <Text fw={700}>{t('guide.page.play.wsrx_title', 'Platform Proxy with Local WSRX')}</Text>
                {config.portMapping === ContainerPortMappingType.PlatformProxy && (
                  <Badge variant="light">{t('guide.page.play.platform_default', 'Platform default')}</Badge>
                )}
              </Group>
              <List type="ordered" spacing={4} size="sm" mt="xs">
                <List.Item>
                  {t(
                    'guide.page.play.wsrx_install',
                    'Download and run WebSocketReflectorX, then select Local WSRX in the instance panel.'
                  )}
                </List.Item>
                <List.Item>
                  {t('guide.page.play.wsrx_ready', 'Wait for Tunnel ready and copy the local 127.0.0.1 address.')}
                </List.Item>
                <List.Item>
                  {t(
                    'guide.page.play.wsrx_use',
                    'Use nc 127.0.0.1 <port> for TCP. A wss:// URL is for WebSocket clients and cannot be passed directly to netcat.'
                  )}
                </List.Item>
                <List.Item>
                  {t(
                    'guide.page.play.wsrx_wss',
                    'Choose WSS only when your tool supports WebSockets and you intentionally need to copy the raw wss:// address.'
                  )}
                </List.Item>
              </List>
              <Anchor href="https://github.com/XDSEC/WebSocketReflectorX/releases" target="_blank" rel="noreferrer">
                {t('guide.page.play.wsrx_download', 'Download WebSocketReflectorX')}
              </Anchor>
            </Card>
            <Card withBorder padding="md">
              <Group justify="space-between" gap="sm" wrap="wrap">
                <Text fw={700}>{t('guide.page.play.vpn_title', 'Event VPN host and port')}</Text>
                <Badge variant="light" color="orange">
                  {t('guide.page.play.event_override', 'Event override')}
                </Badge>
              </Group>
              <Text size="sm" c="dimmed" mt="xs">
                {t(
                  'guide.page.play.vpn_body',
                  'Import the event WireGuard profile and connect first. Start the instance, then use its private host and port while the VPN stays connected.'
                )}
              </Text>
            </Card>
          </Stack>
          <Button component={Link} to={user ? '/challenges' : '/account/login?from=%2Fchallenges'} variant="light">
            {user
              ? t('common.tab.challenge_catalog', 'My challenges')
              : t('guide.page.play.sign_in', 'Sign in to play')}
          </Button>
        </GuideSection>

        <GuideSection
          id="submit-flag"
          number={5}
          icon={mdiFlagOutline}
          title={t('guide.page.submit.title', 'Submit and verify the result')}
          summary={t(
            'guide.page.submit.summary',
            'Paste the exact flag into the challenge submission field and wait for its verdict.'
          )}
        >
          <List spacing="xs" className={classes.list}>
            <List.Item>
              {t('guide.page.submit.exact', 'Do not add quotes, spaces, or command output around the flag.')}
            </List.Item>
            <List.Item>
              {t('guide.page.submit.wait', 'Wait for Accepted or Wrong answer before submitting again.')}
            </List.Item>
            <List.Item>
              {t(
                'guide.page.submit.rules',
                'Keep flags, private instances, accounts, and VPN profiles inside your team.'
              )}
            </List.Item>
          </List>
          <Alert
            icon={<Icon path={mdiInformationOutline} size={0.9} />}
            title={t('guide.page.submit.help_title', 'Need help?')}
          >
            {t(
              'guide.page.submit.help',
              'Read the event notices first. If the platform reports a build, connection, or authorization error, send the exact message and challenge ID to an organizer.'
            )}
          </Alert>
        </GuideSection>
      </Stack>
    </WithNavBar>
  )
}

export default Guide
