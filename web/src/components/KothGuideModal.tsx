import {
  Accordion,
  Anchor,
  Button,
  Code,
  CopyButton,
  Divider,
  Group,
  List,
  Modal,
  ModalProps,
  ScrollArea,
  Stack,
  Text,
  ThemeIcon,
  Title,
} from '@mantine/core'
import { showNotification } from '@mantine/notifications'
import {
  mdiCheck,
  mdiContentCopy,
  mdiCounter,
  mdiCrown,
  mdiHeartPulse,
  mdiRefresh,
  mdiTimerSandComplete,
  mdiToolboxOutline,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import { FC, useMemo } from 'react'
import { useTranslation } from 'react-i18next'
import { useAdToken, AdTokenSection, AdVpnSection, AdTokenRevealModal } from '@Components/AdToolkitSections'
import misc from '@Styles/Misc.module.css'

interface KothToolkitModalProps extends ModalProps {
  gameId: number
}

const TOKEN_RESPONSE_EXAMPLE = `[
  {
    "challengeId": 220,
    "token": "koth_example_token"
  }
]`

const HILLS_RESPONSE_EXAMPLE = `[
  {
    "challengeId": 220,
    "title": "KotH — Blockchain Hill",
    "round": 42,
    "holderParticipationId": 1,
    "holderTeamName": "Team Alpha",
    "provisionalClaimantTeamName": null,
    "provisionalConfirmationTicks": 0,
    "isYou": true,
    "status": "Ok",
    "ip": "172.0.6.65",
    "port": 80,
    "cycleNumber": 7,
    "cycleTick": 2,
    "resetPhase": "Active",
    "nextResetTicks": 1,
    "cooldownParticipants": []
  }
]`

/** A right-aligned "Copy curl" button — so every command snippet is copyable. */
const CopyCurlButton: FC<{ value: string }> = ({ value }) => {
  const { t } = useTranslation()
  return (
    <Group justify="flex-end">
      <CopyButton value={value}>
        {({ copied, copy }) => (
          <Button
            size="compact-xs"
            variant="subtle"
            leftSection={<Icon path={mdiContentCopy} size={0.7} />}
            onClick={copy}
          >
            {copied ? t('game.tooltip.copy.copied', 'Copied') : t('game.button.ad.copy_curl', 'Copy curl')}
          </Button>
        )}
      </CopyButton>
    </Group>
  )
}

/**
 * Player-facing toolkit for King of the Hill. Same shape as
 * <see cref="AdGuideModal"/> — actionable sections (token, VPN) on top,
 * reference docs underneath — but the KotH-specific accordion items are
 * different from A&D: the per-hill capability endpoint + plant flow + state
 * lookup + epoch scoring + crown cycles + champion cooldown.
 *
 * The API token + VPN config are SHARED with A&D (one Bearer token covers
 * both engines, one WG tunnel reaches both bridges) — so this modal calls
 * the same endpoints as AdGuideModal for those sections rather than
 * duplicating state.
 */
export const KothGuideModal: FC<KothToolkitModalProps> = ({ gameId, ...modalProps }) => {
  const { t } = useTranslation()
  const { adTokenHint, rotating, freshToken, storedToken, forgetToken, tokenModalOpen, closeTokenModal, onRotate } =
    useAdToken(gameId, () =>
      showNotification({
        color: 'teal',
        message: t('game.notification.koth.token.rotated', 'KotH token rotated'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
    )

  const apiUrl = `${typeof window !== 'undefined' ? window.location.origin : ''}/api/Game/${gameId}/Ad`
  // Prefer this session's freshly-rotated token, else the one saved in this
  // browser (localStorage) so the curl examples are copy-paste-ready on reload,
  // else the placeholder.
  const exampleBearer = freshToken ?? storedToken ?? '<your-token>'

  const { tokenCurlExample, hillsCurlExample, hillIpCurlExample, plantPseudocode } = useMemo(() => {
    const bearerHeader = `  -H "Authorization: Bearer ${exampleBearer}"`

    return {
      tokenCurlExample: [`curl -sS ${apiUrl}/Koth/Token \\`, bearerHeader].join('\n'),
      // List form — every hill in one call, no challenge id needed. The jq line
      // shows the shape a bot wants: title, where to aim, and who holds it.
      hillsCurlExample: [
        `curl -sS ${apiUrl}/Koth/Hills \\`,
        `${bearerHeader} \\`,
        `  | jq '.[] | {title, ip, port, holder: .holderTeamName, isYou, status}'`,
      ].join('\n'),
      // Same ID-free /Koth/Hills list, projected down to just where-to-aim.
      hillIpCurlExample: [
        `curl -sS ${apiUrl}/Koth/Hills \\`,
        `${bearerHeader} \\`,
        `  | jq '.[] | {hill: .title, ip, port}'`,
      ].join('\n'),
      // Reference plant — players write the selected hill's platform-issued
      // control token verbatim into /koth/king.
      plantPseudocode: `# pseudocode — the actual write path depends on the hill's exploit
CHALLENGE_ID=220
TOKEN=$(curl -sS ${apiUrl}/Koth/Token \\
  -H "Authorization: Bearer ${exampleBearer}" \\
  | jq -r --argjson challenge "$CHALLENGE_ID" \\
      '.[] | select(.challengeId == $challenge) | .token')

# exploit the hill so that this byte string ends up in /koth/king
write_to_hill "/koth/king" "$TOKEN"`,
    }
  }, [apiUrl, exampleBearer])

  return (
    <>
      <Modal
        size="48rem"
        centered
        title={
          <Group gap="sm">
            <ThemeIcon variant="light" color="violet" size="lg">
              <Icon path={mdiToolboxOutline} size={1} />
            </ThemeIcon>
            <Title order={4}>{t('game.content.koth.guide.title', 'King of the Hill — Toolkit')}</Title>
          </Group>
        }
        {...modalProps}
      >
        <ScrollArea h="70vh" scrollbarSize={6}>
          <Stack gap="md" pr="sm">
            <Text size="sm" c="dimmed">
              {t(
                'game.content.koth.guide.intro',
                'Everything you need to play King of the Hill: your API token, the VPN config, the control-token endpoint, and the rules. KotH shares the API token + VPN with A&D — one token, one tunnel, both engines.'
              )}
            </Text>

            <Accordion
              variant="separated"
              defaultValue={['token', 'vpn', 'hill']}
              radius="md"
              chevronPosition="left"
              multiple
            >
              {/* TOKEN — shared with A&D (see AdToolkitSections) */}
              <AdTokenSection
                hint={adTokenHint}
                rotating={rotating}
                onRotate={onRotate}
                storedToken={storedToken}
                onForget={forgetToken}
                title={t('game.content.koth.guide.token.title', 'Your API token')}
                intro={t(
                  'game.content.koth.guide.token.intro',
                  'A personal Bearer token scoped to you + this game. KotH and A&D share it — the same ad_… string authenticates both /Koth/Token and /Submit.'
                )}
                currentLabel={t('game.content.koth.guide.token.current', 'Your current token')}
              />

              {/* VPN — shared with A&D (see AdToolkitSections) */}
              <AdVpnSection
                gameId={gameId}
                title={t('game.content.koth.guide.vpn.title', 'VPN config')}
                intro={t(
                  'game.content.koth.guide.vpn.intro',
                  'Per-user WireGuard config. KotH hills live on the same bridges as A&D services — one tunnel reaches everything. The first download generates a fresh keypair + assigns you an IP from the game subnet; subsequent downloads return the same file.'
                )}
                linuxHint={t(
                  'game.content.koth.guide.vpn.linux_hint',
                  'Linux: sudo wg-quick up ./ad-game-….conf. macOS / Windows: import via the official WireGuard app.'
                )}
              />

              {/* HILL — KotH-specific: cycle capability + plant flow */}
              <Accordion.Item value="hill">
                <Accordion.Control icon={<Icon path={mdiCrown} size={1} color="var(--mantine-color-violet-6)" />}>
                  <Text fw={600}>{t('game.content.koth.guide.hill.title', 'Take the hill')}</Text>
                </Accordion.Control>
                <Accordion.Panel>
                  <Stack gap="sm">
                    <Text size="sm">
                      {t(
                        'game.content.koth.guide.hill.intro',
                        'A KotH challenge is one shared container. Exploit it and write this hill’s current-cycle capability into /koth/king, then keep that capability in control through the required consecutive healthy checks. The first observation is provisional; only a confirmed claim becomes king and earns acquisition credit.'
                      )}
                    </Text>
                    <Text size="sm" fw={600}>
                      {t('game.content.koth.guide.hill.step1', '1. Get your control token')}
                    </Text>
                    <Code block className={misc.ffmono} style={{ fontSize: '0.75rem' }}>
                      {tokenCurlExample}
                    </Code>
                    <Group justify="space-between">
                      <Text size="xs" c="dimmed">
                        {t(
                          'game.content.koth.guide.hill.token_note',
                          'Fetch a fresh token after each crown-cycle reset. Old tokens are revoked before the replacement hill becomes active and can never claim the new container.'
                        )}
                      </Text>
                      <CopyButton value={tokenCurlExample}>
                        {({ copied, copy }) => (
                          <Button
                            size="compact-xs"
                            variant="subtle"
                            leftSection={<Icon path={mdiContentCopy} size={0.7} />}
                            onClick={copy}
                          >
                            {copied
                              ? t('game.tooltip.copy.copied', 'Copied')
                              : t('game.button.ad.copy_curl', 'Copy curl')}
                          </Button>
                        )}
                      </CopyButton>
                    </Group>
                    <Code block className={misc.ffmono} style={{ fontSize: '0.75rem' }}>
                      {TOKEN_RESPONSE_EXAMPLE}
                    </Code>
                    <Text size="xs" c="dimmed">
                      {t(
                        'game.content.koth.guide.hill.status_note',
                        'The response contains one entry per enabled hill. It is empty during warmup; each entry carries that hill’s exact token for the current crown cycle.'
                      )}
                    </Text>

                    <Divider />

                    <Text size="sm" fw={600}>
                      {t('game.content.koth.guide.hill.step2', '2. Plant it on the hill')}
                    </Text>
                    <Text size="sm">
                      {t(
                        'game.content.koth.guide.hill.plant_intro',
                        'How you get the bytes into /koth/king is the actual KotH challenge — exploit the hill’s service to write the file. The platform doesn’t care HOW you got it there, only what’s there when the checker peeks.'
                      )}
                    </Text>
                    <Code block className={misc.ffmono} style={{ fontSize: '0.75rem' }}>
                      {plantPseudocode}
                    </Code>
                    <CopyCurlButton value={plantPseudocode} />
                    <Text size="xs" c="dimmed">
                      {t(
                        'game.content.koth.guide.hill.last_write_wins',
                        'A single fast write is not enough. Keep your exact token in place while the service stays healthy for every confirmation tick; a rival token or a Mumble/Offline verdict breaks the streak.'
                      )}
                    </Text>

                    <Divider />

                    <Text size="sm" fw={600}>
                      {t('game.content.koth.guide.hill.step3', '3. Find the hill IP:port')}
                    </Text>
                    <Code block className={misc.ffmono} style={{ fontSize: '0.75rem' }}>
                      {hillIpCurlExample}
                    </Code>
                    <CopyCurlButton value={hillIpCurlExample} />
                    <Text size="xs" c="dimmed">
                      {t(
                        'game.content.koth.guide.hill.targets_note',
                        'GET /Koth/Hills returns every live target. The container is destroyed and recreated from the same pristine image at each crown-cycle boundary, so always re-read the exact endpoint after a reset.'
                      )}
                    </Text>
                  </Stack>
                </Accordion.Panel>
              </Accordion.Item>

              {/* STATE — current holder + verdict */}
              <Accordion.Item value="state">
                <Accordion.Control icon={<Icon path={mdiHeartPulse} size={1} color="var(--mantine-color-blue-6)" />}>
                  <Text fw={600}>{t('game.content.koth.guide.state.title', 'Did my plant take?')}</Text>
                </Accordion.Control>
                <Accordion.Panel>
                  <Stack gap="sm">
                    <Text size="sm">
                      {t(
                        'game.content.koth.guide.state.intro',
                        'GET /Koth/Hills returns every hill, its exact current endpoint, confirmed king, provisional claimant and progress, checker verdict, crown-cycle position, reset phase, and cooldown. Use it as the authoritative input to your automation.'
                      )}
                    </Text>
                    <Code block className={misc.ffmono} style={{ fontSize: '0.75rem' }}>
                      {hillsCurlExample}
                    </Code>
                    <Group justify="flex-end">
                      <CopyButton value={hillsCurlExample}>
                        {({ copied, copy }) => (
                          <Button
                            size="compact-xs"
                            variant="subtle"
                            leftSection={<Icon path={mdiContentCopy} size={0.7} />}
                            onClick={copy}
                          >
                            {copied
                              ? t('game.tooltip.copy.copied', 'Copied')
                              : t('game.button.ad.copy_curl', 'Copy curl')}
                          </Button>
                        )}
                      </CopyButton>
                    </Group>
                    <Code block className={misc.ffmono} style={{ fontSize: '0.75rem' }}>
                      {HILLS_RESPONSE_EXAMPLE}
                    </Code>
                    <Text size="xs" c="dimmed">
                      {t('game.content.koth.guide.state.fields', {
                        gameId,
                        defaultValue:
                          'holderTeamName is the confirmed king; provisionalClaimantTeamName is still proving control. Reset/readiness phases are non-scorable. For a single hill, GET /api/game/{{gameId}}/ad/koth/{challenge-id}/state.',
                      })}
                    </Text>
                  </Stack>
                </Accordion.Panel>
              </Accordion.Item>

              {/* SCORING — KotH formulas */}
              <Accordion.Item value="scoring">
                <Accordion.Control icon={<Icon path={mdiCounter} size={1} color="var(--mantine-color-teal-6)" />}>
                  <Text fw={600}>{t('game.content.koth.guide.scoring.title', 'How scoring works')}</Text>
                </Accordion.Control>
                <Accordion.Panel>
                  <Stack gap="sm">
                    <Text size="sm">
                      {t(
                        'game.content.koth.guide.epoch_scoring.intro',
                        'Each checker tick contributes evidence to a fixed-length epoch. A team’s score on each hill combines three rates:'
                      )}
                    </Text>
                    <List size="sm" spacing="xs">
                      <List.Item
                        icon={
                          <Text c="teal" fw={700} ff="monospace">
                            A
                          </Text>
                        }
                      >
                        <Text component="span" fw={600} c="teal">
                          {t('game.content.koth.guide.epoch_scoring.acquisition_label', 'Acquisition')}:
                        </Text>{' '}
                        {t(
                          'game.content.koth.guide.epoch_scoring.acquisition',
                          'the share of eligible crown-cycle capability windows in which your team proves control of the hill.'
                        )}
                      </List.Item>
                      <List.Item
                        icon={
                          <Text c="blue" fw={700} ff="monospace">
                            C
                          </Text>
                        }
                      >
                        <Text component="span" fw={600} c="blue">
                          {t('game.content.koth.guide.epoch_scoring.control_label', 'Control')}:
                        </Text>{' '}
                        {t(
                          'game.content.koth.guide.epoch_scoring.control',
                          'the share of scorable checker ticks during which your marker controls the hill.'
                        )}
                      </List.Item>
                      <List.Item
                        icon={
                          <Text c="orange" fw={700} ff="monospace">
                            R
                          </Text>
                        }
                      >
                        <Text component="span" fw={600} c="orange">
                          {t('game.content.koth.guide.epoch_scoring.reliability_label', 'Reliability')}:
                        </Text>{' '}
                        {t(
                          'game.content.koth.guide.epoch_scoring.sla',
                          'healthy responsible ticks divided by responsible ticks. A team with no responsibility has zero reliability and zero points; InternalError and platform failures are void.'
                        )}
                      </List.Item>
                    </List>
                    <Divider />
                    <Code block className={misc.ffmono} style={{ fontSize: '0.78rem' }}>
                      {'Hill = 100 × R × (0.25A + 0.55C + 0.20√(A×C))'}
                    </Code>
                    <Text size="sm">
                      {t(
                        'game.content.koth.guide.epoch_scoring.total',
                        'Hill scores are normalized by service weight into one fixed-ceiling epoch score. Official rank uses finalized epochs; the unfinished epoch is shown only as a live projection. There are no flat hold credits or negative point penalties.'
                      )}
                    </Text>
                  </Stack>
                </Accordion.Panel>
              </Accordion.Item>

              {/* REFRESH + COOLDOWN */}
              <Accordion.Item value="refresh">
                <Accordion.Control
                  icon={<Icon path={mdiTimerSandComplete} size={1} color="var(--mantine-color-blue-6)" />}
                >
                  <Text fw={600}>
                    {t('game.content.koth.guide.refresh.title', 'Crown cycles, pristine resets & cooldown')}
                  </Text>
                </Accordion.Control>
                <Accordion.Panel>
                  <List size="sm" spacing={4}>
                    <List.Item icon={<Icon path={mdiRefresh} size={0.8} color="var(--mantine-color-blue-6)" />}>
                      {t(
                        'game.content.koth.guide.refresh.wipe',
                        'At every crown-cycle boundary, the old shared container is destroyed and exactly one replacement is created from the same pristine challenge image. Footholds, markers, and patches do not carry over.'
                      )}
                    </List.Item>
                    <List.Item icon={<Icon path={mdiCrown} size={0.8} color="var(--mantine-color-violet-6)" />}>
                      {t(
                        'game.content.koth.guide.refresh.cooldown',
                        'The team with the most confirmed healthy controlled ticks in the previous cycle is blocked from this hill for the configured opening tick. Every tied leader cools down unless that would leave no challenger.'
                      )}
                    </List.Item>
                    <List.Item>
                      {t(
                        'game.content.koth.guide.refresh.recent_window',
                        'Cooldown starts only after readiness succeeds, expires by authoritative scoring rounds, and its forced tick is removed from that team’s personal eligible denominator.'
                      )}
                    </List.Item>
                    <List.Item>
                      {t(
                        'game.content.koth.guide.refresh.epoch_timing_tip',
                        'Scoring and attribution pause through finalize, destroy, create, and readiness. Reset time and platform failures are excluded for every team.'
                      )}
                    </List.Item>
                  </List>
                </Accordion.Panel>
              </Accordion.Item>

              {/* DO / DON'T */}
              <Accordion.Item value="do_dont">
                <Accordion.Control icon={<Icon path={mdiCounter} size={1} color="var(--mantine-color-gray-6)" />}>
                  <Text fw={600}>{t('game.content.koth.guide.do_dont.title', "Do / Don't")}</Text>
                </Accordion.Control>
                <Accordion.Panel>
                  <Stack gap="xs">
                    <Text size="sm" fw={600} c="teal">
                      {t('game.content.koth.guide.do_dont.do', 'Do')}
                    </Text>
                    <List size="sm" spacing={2}>
                      <List.Item>
                        {t(
                          'game.content.koth.guide.do_dont.do_script_epoch',
                          'Script the per-hill capability-fetch + plant loop. Manual planting means missed acquisition windows and control evidence.'
                        )}
                      </List.Item>
                      <List.Item>
                        {t(
                          'game.content.koth.guide.do_dont.do_plant_late',
                          'Keep the same current-cycle capability in control through every healthy confirmation check. A rival capability or unhealthy verdict restarts the streak.'
                        )}
                      </List.Item>
                      <List.Item>
                        {t(
                          'game.content.koth.guide.do_dont.do_repatch',
                          'Have re-exploit and re-patch automation ready for each crown-cycle reset. Patching is encouraged, but patches survive only until the next reset.'
                        )}
                      </List.Item>
                      <List.Item>
                        {t(
                          'game.content.koth.guide.do_dont.do_watch_status_epoch',
                          'Watch the hill status and claim-progress badges. Mumble or Offline breaks confirmation and reduces reliability while your team is responsible.'
                        )}
                      </List.Item>
                    </List>
                    <Text size="sm" fw={600} c="red" mt="xs">
                      {t('game.content.koth.guide.do_dont.dont', "Don't")}
                    </Text>
                    <List size="sm" spacing={2}>
                      <List.Item>
                        {t(
                          'game.content.koth.guide.do_dont.dont_stale_token',
                          'Plant a capability from an earlier crown cycle — it is revoked at reset and cannot claim the replacement hill.'
                        )}
                      </List.Item>
                      <List.Item>
                        {t(
                          'game.content.koth.guide.do_dont.dont_replay',
                          'Plant a token you observed in /koth/king from another team — it’ll credit THEM, not you. (You’re also flagged in the audit log.)'
                        )}
                      </List.Item>
                      <List.Item>
                        {t(
                          'game.content.koth.guide.do_dont.dont_dos_epoch',
                          'DoS the hill. Broken ticks damage scoring evidence, organizers can see the traffic, and deliberate disruption can lead to disqualification.'
                        )}
                      </List.Item>
                      <List.Item>
                        {t(
                          'game.content.koth.guide.do_dont.dont_persist',
                          'Try to persist tricks across a crown-cycle reset. The old container is completely destroyed; the replacement starts from the configured pristine image.'
                        )}
                      </List.Item>
                    </List>
                  </Stack>
                </Accordion.Panel>
              </Accordion.Item>
            </Accordion>

            <Text size="xs" c="dimmed" ta="center">
              {t('game.content.koth.guide.footer', 'Endpoint base: ')}
              <Anchor href={apiUrl} target="_blank" rel="noreferrer" className={misc.ffmono}>
                {apiUrl}
              </Anchor>
            </Text>
          </Stack>
        </ScrollArea>
      </Modal>

      {/* Fresh-token reveal — shared with A&D (see AdToolkitSections). */}
      <AdTokenRevealModal
        opened={tokenModalOpen}
        onClose={closeTokenModal}
        freshToken={freshToken}
        title={t('game.content.koth.token_modal.title', 'Your new API token (KotH + A&D)')}
        warning={t(
          'game.content.koth.token_modal.warning',
          'This token is now saved in this browser (see “Saved token” in the API-token section) so your scripts can reuse it. Copy it here too if you want it elsewhere — the platform keeps only a hash and can’t show it again. The previous token (if any) has been invalidated. The same token authenticates both /Koth/Token and /Submit.'
        )}
      />
    </>
  )
}
