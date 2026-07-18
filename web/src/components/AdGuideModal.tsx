import {
  Accordion,
  Alert,
  Anchor,
  Box,
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
  Tabs,
  Text,
  Textarea,
  ThemeIcon,
  Title,
} from '@mantine/core'
import { useDisclosure } from '@mantine/hooks'
import { showNotification } from '@mantine/notifications'
import {
  mdiAlertCircleOutline,
  mdiCheck,
  mdiConsole,
  mdiContentCopy,
  mdiCounter,
  mdiCubeOutline,
  mdiDelete,
  mdiDownload,
  mdiKeyChain,
  mdiKeyOutline,
  mdiRestart,
  mdiShieldHalfFull,
  mdiSwordCross,
  mdiToolboxOutline,
  mdiUpload,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { useAdToken, AdTokenSection, AdVpnSection, AdTokenRevealModal } from '@Components/AdToolkitSections'
import { showErrorMsg } from '@Utils/Shared'
import api from '@Api'
import misc from '@Styles/Misc.module.css'

interface AdToolkitModalProps extends ModalProps {
  gameId: number
}

/**
 * Player-facing toolkit for Attack &amp; Defense. Single modal that bundles
 * the actionable pieces (team API token, VPN config download) with the
 * reference docs (rules, scoring math, container ops, do/don't). Renamed
 * from "Player Guide" because half the content is interactive rather than
 * read-only.
 */
export const AdGuideModal: FC<AdToolkitModalProps> = ({ gameId, ...modalProps }) => {
  const { t } = useTranslation()
  const { adTokenHint, rotating, freshToken, storedToken, forgetToken, tokenModalOpen, closeTokenModal, onRotate } =
    useAdToken(gameId)
  const { data: sshKey, mutate: mutateSshKey } = api.game.useAdGameGetSshKey(gameId)

  const [sshTab, setSshTab] = useState<string>('paste')
  const [pastedPubkey, setPastedPubkey] = useState('')
  const [sshBusy, setSshBusy] = useState(false)
  const [freshPrivKey, setFreshPrivKey] = useState<{
    privateKey: string
    publicKey: string
    fingerprint: string
  } | null>(null)
  const [privKeyModalOpen, { open: openPrivKeyModal, close: closePrivKeyModal }] = useDisclosure(false)

  const onUploadSshKey = async () => {
    if (!pastedPubkey.trim()) return
    setSshBusy(true)
    try {
      await api.game.adGameUploadSshKey(gameId, { publicKey: pastedPubkey.trim() })
      setPastedPubkey('')
      mutateSshKey()
      showNotification({
        color: 'teal',
        message: t('game.notification.ad.ssh.uploaded', 'SSH public key registered'),
        icon: <Icon path={mdiCheck} size={1} />,
      })
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setSshBusy(false)
    }
  }

  const onGenerateSshKey = async () => {
    setSshBusy(true)
    try {
      const { data } = await api.game.adGameGenerateSshKey(gameId)
      setFreshPrivKey({ privateKey: data.privateKey, publicKey: data.publicKey, fingerprint: data.fingerprint })
      openPrivKeyModal()
      mutateSshKey()
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setSshBusy(false)
    }
  }

  const onRevokeSshKey = async () => {
    setSshBusy(true)
    try {
      await api.game.adGameRevokeSshKey(gameId)
      mutateSshKey()
      showNotification({
        color: 'orange',
        message: t('game.notification.ad.ssh.revoked', 'SSH key revoked'),
      })
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setSshBusy(false)
    }
  }

  const downloadPrivKey = () => {
    if (!freshPrivKey) return
    const blob = new Blob([freshPrivKey.privateKey], { type: 'application/x-pem-file' })
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = `rsctf-ad-game${gameId}.key`
    a.click()
    URL.revokeObjectURL(url)
  }

  const jumpHost = sshKey?.jumpHost ?? 'host:22022'
  const sshExample = `ssh <challenge-id>@${jumpHost.split(':')[0]} -p ${jumpHost.split(':')[1] ?? '22022'} -i ~/.ssh/your-key`

  const apiUrl = `${typeof window !== 'undefined' ? window.location.origin : ''}/api/Game/${gameId}/Ad`

  // The DB stores only an HMAC hash of the token, so the live UI never
  // has the plaintext after the rotation modal closes — only the
  // `ad_aDQR…ykTM` recognizer hint. Putting that hint into the curl
  // example is misleading (users copy + paste and get 401). Use the
  // session-fresh plaintext if we still have it; otherwise emit a
  // placeholder + a note so it's obvious they need to drop their saved
  // token in.
  const exampleBearer = freshToken ?? storedToken ?? '<your-token>'

  const curlExample = [
    `curl -X POST ${apiUrl}/Submit \\`,
    `  -H "Authorization: Bearer ${exampleBearer}" \\`,
    `  -H "Content-Type: application/json" \\`,
    `  -d '{"flags":["flag{captured_from_team_b}","flag{another}"]}'`,
  ].join('\n')

  const responseExample = `{
  "acceptedCount": 1,
  "results": [
    { "flag": "flag{captured_from_team_b}",
      "status": "accepted",
      "flagPlantedAtRound": 7 },
    { "flag": "flag{another}",
      "status": "wrong",
      "message": "flag not recognized" }
  ]
}`

  const targetsCurlExample = [`curl -sS ${apiUrl}/Targets \\`, `  -H "Authorization: Bearer ${exampleBearer}"`].join(
    '\n'
  )

  const targetsResponseExample = `{
  "currentRound": 7,
  "challenges": [
    {
      "challengeId": 76,
      "title": "Test A&D — nginx",
      "tickSeconds": 60,
      "teams": [
        { "participationId": 2, "teamName": "Team Bravo",
          "division": null, "ip": "172.0.9.4", "port": 80,
          "lastCheckStatus": "Ok" },
        { "participationId": 3, "teamName": "Team Charlie",
          "division": null, "ip": "172.0.9.5", "port": 80,
          "lastCheckStatus": "Mumble" }
      ]
    }
  ]
}`

  return (
    <>
      <Modal
        size="48rem"
        centered
        title={
          <Group gap="sm">
            <ThemeIcon variant="light" color="red" size="lg">
              <Icon path={mdiToolboxOutline} size={1} />
            </ThemeIcon>
            <Title order={4}>{t('game.content.ad.guide.title', 'Attack & Defense — Toolkit')}</Title>
          </Group>
        }
        {...modalProps}
      >
        <ScrollArea h="70vh" scrollbarSize={6}>
          <Stack gap="md" pr="sm">
            <Text size="sm" c="dimmed">
              {t(
                'game.content.ad.guide.intro',
                'Everything you need to play A&D: your personal API token, your VPN config, the API contract, and the rules. The first two sections are actionable — token rotation and VPN config download.'
              )}
            </Text>

            <Accordion variant="separated" defaultValue={['token', 'vpn']} radius="md" chevronPosition="left" multiple>
              {/* TOKEN — shared with KotH (see AdToolkitSections) */}
              <AdTokenSection
                hint={adTokenHint}
                rotating={rotating}
                onRotate={onRotate}
                storedToken={storedToken}
                onForget={forgetToken}
                title={t('game.content.ad.guide.token.title', 'Your API token')}
                intro={t(
                  'game.content.ad.guide.token.intro',
                  'A personal Bearer token scoped to you + this game. Your exploit scripts pass it as Authorization: Bearer <token> when submitting captured flags. Every team member manages their own — rotating yours does not affect anyone else, and if you get kicked from the team your token stops working immediately.'
                )}
                currentLabel={t('game.content.ad.guide.token.current', 'Your current token')}
              />

              {/* VPN — shared with KotH (see AdToolkitSections) */}
              <AdVpnSection
                gameId={gameId}
                title={t('game.content.ad.guide.vpn.title', 'VPN config')}
                intro={t(
                  'game.content.ad.guide.vpn.intro',
                  'Per-user WireGuard config. The first download generates a fresh keypair + assigns you an IP from the game subnet; subsequent downloads return the same file. Drop the .conf into wg-quick (or wireguard-tools / the WireGuard app) to join the A&D network.'
                )}
                linuxHint={t(
                  'game.content.ad.guide.vpn.linux_hint',
                  'Linux: sudo wg-quick up ./ad-game-….conf. macOS / Windows: import via the official WireGuard app.'
                )}
              />

              {/* SSH */}
              <Accordion.Item value="ssh">
                <Accordion.Control icon={<Icon path={mdiConsole} size={1} color="var(--mantine-color-grape-6)" />}>
                  <Text fw={600}>{t('game.content.ad.guide.ssh.title', 'Shell access (SSH)')}</Text>
                </Accordion.Control>
                <Accordion.Panel>
                  <Stack gap="sm">
                    <Text size="sm">
                      {t(
                        'game.content.ad.guide.ssh.intro',
                        "Direct shell into your team's container for any A&D challenge — patch the binary, tail logs, read /flag, install tools. Auth is your SSH key, identity is the SSH username (= the challenge id)."
                      )}
                    </Text>

                    <Group justify="space-between" wrap="wrap" gap="xs">
                      <Group gap="xs">
                        <Text size="sm" fw={600}>
                          {t('game.content.ad.guide.ssh.current', 'Your registered key')}:
                        </Text>
                        {sshKey?.exists ? (
                          <Group gap={4}>
                            <Text size="sm" className={misc.ffmono}>
                              {sshKey.fingerprint}
                            </Text>
                            <Text size="xs" c="dimmed">
                              ({sshKey.algorithm}
                              {sshKey.platformGenerated &&
                                t('game.content.ad.guide.ssh.generated_tag', ', platform-generated')}
                              )
                            </Text>
                          </Group>
                        ) : (
                          <Text size="sm" c="dimmed">
                            {t('game.content.ad.guide.ssh.no_key', 'No key yet')}
                          </Text>
                        )}
                      </Group>
                      {sshKey?.exists && (
                        <Button
                          size="xs"
                          variant="default"
                          color="red"
                          leftSection={<Icon path={mdiDelete} size={0.7} />}
                          loading={sshBusy}
                          onClick={onRevokeSshKey}
                        >
                          {t('game.button.ad.ssh.revoke', 'Revoke')}
                        </Button>
                      )}
                    </Group>
                    {sshKey?.exists && sshKey.lastUsedAt && (
                      <Text size="xs" c="dimmed">
                        {t('game.content.ad.last_used', 'Last used')}: {dayjs(sshKey.lastUsedAt).fromNow()}
                      </Text>
                    )}

                    <Divider />

                    <Tabs value={sshTab} onChange={(v) => v && setSshTab(v)}>
                      <Tabs.List>
                        <Tabs.Tab value="paste" leftSection={<Icon path={mdiKeyOutline} size={0.8} />}>
                          {t('game.content.ad.guide.ssh.tab_paste', 'Paste public key')}
                        </Tabs.Tab>
                        <Tabs.Tab value="generate" leftSection={<Icon path={mdiKeyChain} size={0.8} />}>
                          {t('game.content.ad.guide.ssh.tab_generate', 'Generate keypair')}
                        </Tabs.Tab>
                      </Tabs.List>

                      <Tabs.Panel value="paste" pt="sm">
                        <Stack gap="xs">
                          <Text size="xs" c="dimmed">
                            {t(
                              'game.content.ad.guide.ssh.paste_hint',
                              'Run `cat ~/.ssh/id_ed25519.pub` (or id_rsa.pub) locally and paste the single line below. The private half never leaves your machine.'
                            )}
                          </Text>
                          <Textarea
                            label={t('game.content.ad.guide.ssh.public_key_label', 'SSH public key')}
                            value={pastedPubkey}
                            onChange={(e) => setPastedPubkey(e.currentTarget.value)}
                            placeholder="ssh-ed25519 AAAAC3NzaC1lZDI1NTE5... your-comment"
                            minRows={2}
                            maxRows={4}
                            autosize
                            styles={{ input: { fontFamily: 'monospace', fontSize: '0.75rem' } }}
                          />
                          <Group justify="flex-end">
                            <Button
                              size="xs"
                              leftSection={<Icon path={mdiUpload} size={0.8} />}
                              loading={sshBusy}
                              disabled={!pastedPubkey.trim()}
                              onClick={onUploadSshKey}
                            >
                              {sshKey?.exists
                                ? t('game.button.ad.ssh.replace', 'Replace')
                                : t('game.button.ad.ssh.upload', 'Upload')}
                            </Button>
                          </Group>
                        </Stack>
                      </Tabs.Panel>

                      <Tabs.Panel value="generate" pt="sm">
                        <Stack gap="xs">
                          <Text size="xs" c="dimmed">
                            {t(
                              'game.content.ad.guide.ssh.generate_hint',
                              "Less secure (the private key crosses the network once). Useful if you don't have ssh-keygen locally, e.g. on Windows without WSL. Downloads a .key file you save and pass to ssh -i."
                            )}
                          </Text>
                          <Group justify="flex-end">
                            <Button
                              size="xs"
                              color="grape"
                              leftSection={<Icon path={mdiKeyChain} size={0.8} />}
                              loading={sshBusy}
                              onClick={onGenerateSshKey}
                            >
                              {t('game.button.ad.ssh.generate', 'Generate ed25519 keypair')}
                            </Button>
                          </Group>
                        </Stack>
                      </Tabs.Panel>
                    </Tabs>

                    <Divider />

                    <Text size="sm" fw={600}>
                      {t('game.content.ad.guide.ssh.connect', 'Connect')}
                    </Text>
                    <Code block className={misc.ffmono} style={{ fontSize: '0.75rem' }}>
                      {sshExample}
                    </Code>
                    <Group justify="flex-end">
                      <CopyButton value={sshExample}>
                        {({ copied, copy }) => (
                          <Button
                            size="compact-xs"
                            variant="subtle"
                            leftSection={<Icon path={mdiContentCopy} size={0.7} />}
                            onClick={copy}
                          >
                            {copied ? t('game.tooltip.copy.copied', 'Copied') : t('common.button.copy', 'Copy')}
                          </Button>
                        )}
                      </CopyButton>
                    </Group>
                    <Text size="xs" c="dimmed">
                      {t(
                        'game.content.ad.guide.ssh.connect_hint',
                        'Replace <challenge-id> with the numeric id from the challenge card (visible in the URL and the "SSH access" hint inside each challenge). The username doubles as the target selector — `ssh 76@host` lands in your team\'s container for challenge 76.'
                      )}
                    </Text>
                  </Stack>
                </Accordion.Panel>
              </Accordion.Item>

              {/* ROUNDS */}
              <Accordion.Item value="rounds">
                <Accordion.Control icon={<Icon path={mdiSwordCross} size={1} color="var(--mantine-color-red-6)" />}>
                  <Text fw={600}>{t('game.content.ad.guide.rounds.title', 'Rounds & flags')}</Text>
                </Accordion.Control>
                <Accordion.Panel>
                  <List size="sm" spacing={4}>
                    <List.Item>
                      {t(
                        'game.content.ad.guide.rounds.tick',
                        'A round (tick) is the scoring unit. Length is set once for the whole event (the operator sets it — typically 60–180s).'
                      )}
                    </List.Item>
                    <List.Item>
                      {t(
                        'game.content.ad.guide.rounds.plant',
                        'At each tick the platform writes a fresh flag into your container (the "current flag"). You can see it in the challenge modal — protect it.'
                      )}
                    </List.Item>
                    <List.Item>
                      {t(
                        'game.content.ad.guide.rounds.lifetime',
                        'Old flags stay valid for N ticks (default 5). After that they expire and cannot be accepted as scoring evidence.'
                      )}
                    </List.Item>
                    <List.Item>
                      {t(
                        'game.content.ad.guide.rounds.warmup',
                        'Round 0 is warmup — no scoring yet, just make sure your service is up.'
                      )}
                    </List.Item>
                  </List>
                </Accordion.Panel>
              </Accordion.Item>

              {/* SCORING */}
              <Accordion.Item value="scoring">
                <Accordion.Control icon={<Icon path={mdiCounter} size={1} color="var(--mantine-color-teal-6)" />}>
                  <Text fw={600}>{t('game.content.ad.guide.scoring.title', 'How scoring works')}</Text>
                </Accordion.Control>
                <Accordion.Panel>
                  <Stack gap="sm">
                    <Text size="sm">
                      {t(
                        'game.content.ad.guide.scoring.intro',
                        'The official scoreboard settles accepted outcomes per service inside equal-budget epochs:'
                      )}
                    </Text>
                    <List size="sm" spacing="xs">
                      <List.Item icon={<Icon path={mdiSwordCross} size={0.8} color="var(--mantine-color-teal-6)" />}>
                        <Text component="span" fw={600} c="teal">
                          {t('game.content.ad.guide.scoring.attack_label', 'Attack')}:
                        </Text>{' '}
                        <Code className={misc.ffmono}>A = min(1, C + 0.25H)</Code>{' '}
                        {t(
                          'game.content.ad.guide.scoring.attack',
                          'C is your accepted-capture coverage over the frozen opponent roster. H adds a bounded capturer-count rarity fraction. An accepted submission records evidence immediately, but it has no standalone point value; the epoch settles all outcomes together.'
                        )}
                      </List.Item>
                      <List.Item icon={<Icon path={mdiShieldHalfFull} size={0.8} color="var(--mantine-color-red-6)" />}>
                        <Text component="span" fw={600} c="red">
                          {t('game.content.ad.guide.scoring.defense_label', 'Defense')}:
                        </Text>{' '}
                        <Code className={misc.ffmono}>D = protected pairs / eligible pairs</Code>.{' '}
                        {t(
                          'game.content.ad.guide.scoring.defense',
                          'Every exact healthy custom-check flag creates one pair per frozen opponent. Each distinct capturer removes only its own protected pair. An unstolen pair is observable non-capture, not proof that an exploit was attempted.'
                        )}
                      </List.Item>
                      <List.Item icon={<Icon path={mdiCounter} size={0.8} color="var(--mantine-color-blue-6)" />}>
                        <Text component="span" fw={600} c="blue">
                          {t('game.content.ad.guide.scoring.sla_label', 'SLA')}:
                        </Text>{' '}
                        <Code className={misc.ffmono}>R = checker credit / service ticks</Code>.{' '}
                        {t(
                          'game.content.ad.guide.scoring.sla',
                          'A clean passing check earns 1.0, recovery earns 0.5, and a failed or missing check earns 0. InternalError carries the last scored non-infrastructure result after startRound; if none exists, that challenge-round sample is void for the full roster.'
                        )}
                      </List.Item>
                    </List>
                    <Divider />
                    <Text size="sm" fw={600}>
                      {t(
                        'game.content.ad.guide.scoring.total_formula',
                        'Service = 100 × R × (0.40A + 0.40D + 0.20√(A×D)); normalized service weights form the epoch score, and finalized epochs determine official rank.'
                      )}
                    </Text>
                  </Stack>
                </Accordion.Panel>
              </Accordion.Item>

              {/* TARGETS */}
              <Accordion.Item value="targets">
                <Accordion.Control icon={<Icon path={mdiSwordCross} size={1} color="var(--mantine-color-red-6)" />}>
                  <Text fw={600}>{t('game.content.ad.guide.targets.title', 'Find your targets')}</Text>
                </Accordion.Control>
                <Accordion.Panel>
                  <Stack gap="sm">
                    <Text size="sm">
                      {t(
                        'game.content.ad.guide.targets.intro',
                        "A&D is attack-first — you need to know every other team's container IP per service. This endpoint is the canonical list. Excludes your own team and waits until the warmup round has elapsed (currentRound > 0)."
                      )}
                    </Text>
                    <Text size="sm" fw={600}>
                      {t('game.content.ad.guide.targets.request', 'List request')}
                    </Text>
                    <Code block className={misc.ffmono} style={{ fontSize: '0.75rem' }}>
                      {targetsCurlExample}
                    </Code>
                    {!freshToken && adTokenHint?.exists && (
                      <Text size="xs" c="orange">
                        {t(
                          'game.content.ad.guide.token_placeholder_note',
                          'Replace <your-token> with the full ad_… string shown when you rotated. The hint above (e.g. ad_aDQR…ykTM) is just a recognizer — the server never has the full token after rotation.'
                        )}
                      </Text>
                    )}
                    <Group justify="space-between">
                      <Text size="sm" c="dimmed">
                        {t(
                          'game.content.ad.guide.targets.poll_note',
                          'Poll once per round (~tickSeconds). Container IPs are stable across rounds unless a team Resets or an admin Stops the container — then a fresh IP shows up here within ~15s.'
                        )}
                      </Text>
                      <CopyButton value={targetsCurlExample}>
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
                    <Text size="sm" fw={600}>
                      {t('game.content.ad.guide.targets.response', 'Response shape')}
                    </Text>
                    <Code block className={misc.ffmono} style={{ fontSize: '0.75rem' }}>
                      {targetsResponseExample}
                    </Code>
                    <Text size="sm" c="dimmed">
                      {t(
                        'game.content.ad.guide.targets.tip',
                        "lastCheckStatus tells you which targets are healthy: Ok / Mumble / Offline. Prioritize Ok ones — Offline containers won't accept your exploit, and Mumble usually means the service is broken in a way that won't expose the flag."
                      )}
                    </Text>
                  </Stack>
                </Accordion.Panel>
              </Accordion.Item>

              {/* SUBMIT */}
              <Accordion.Item value="submit">
                <Accordion.Control icon={<Icon path={mdiContentCopy} size={1} color="var(--mantine-color-grape-6)" />}>
                  <Text fw={600}>{t('game.content.ad.guide.submit.title', 'How to submit captured flags')}</Text>
                </Accordion.Control>
                <Accordion.Panel>
                  <Stack gap="sm">
                    <Text size="sm">
                      {t(
                        'game.content.ad.guide.submit.api_only',
                        'Flag submission is API-only — there is no web form for A&D. Your exploit scripts batch the flags they collected and POST them in one request.'
                      )}
                    </Text>
                    <Text size="sm" fw={600}>
                      {t('game.content.ad.guide.submit.request', 'Submit request')}
                    </Text>
                    <Code block className={misc.ffmono} style={{ fontSize: '0.75rem' }}>
                      {curlExample}
                    </Code>
                    {!freshToken && adTokenHint?.exists && (
                      <Text size="xs" c="orange">
                        {t(
                          'game.content.ad.guide.token_placeholder_note',
                          'Replace <your-token> with the full ad_… string shown when you rotated. The hint above (e.g. ad_aDQR…ykTM) is just a recognizer — the server never has the full token after rotation.'
                        )}
                      </Text>
                    )}
                    <Group justify="space-between">
                      <Text size="sm" c="dimmed">
                        {t(
                          'game.content.ad.guide.submit.batch_note',
                          'Pass 1 to 100 flag strings per request. Results return in input order so you can correlate.'
                        )}
                      </Text>
                      <CopyButton value={curlExample}>
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
                    <Text size="sm" fw={600}>
                      {t('game.content.ad.guide.submit.response', 'Response shape')}
                    </Text>
                    <Code block className={misc.ffmono} style={{ fontSize: '0.75rem' }}>
                      {responseExample}
                    </Code>
                    <Text size="sm" c="dimmed">
                      {t(
                        'game.content.ad.guide.submit.statuses',
                        'Per-flag status is one of: accepted | duplicate | wrong | expired | self_attack | not_started | ended | paused | rejected. Accepted flags become epoch evidence; the submit response does not assign immediate points.'
                      )}
                    </Text>
                  </Stack>
                </Accordion.Panel>
              </Accordion.Item>

              {/* CONTAINER */}
              <Accordion.Item value="container">
                <Accordion.Control icon={<Icon path={mdiCubeOutline} size={1} color="var(--mantine-color-violet-6)" />}>
                  <Text fw={600}>{t('game.content.ad.guide.container.title', 'Your container')}</Text>
                </Accordion.Control>
                <Accordion.Panel>
                  <List size="sm" spacing={4}>
                    <List.Item>
                      {t(
                        'game.content.ad.guide.container.ip',
                        "The challenge modal shows your container IP:port — that's where checks run and where attackers point their exploits."
                      )}
                    </List.Item>
                    <List.Item>
                      {t(
                        'game.content.ad.guide.container.flag_file',
                        "The live flag is rewritten every round at the path in the RSCTF_FLAG_FILE environment variable. Read it fresh from $RSCTF_FLAG_FILE on every request (don't cache it)."
                      )}
                    </List.Item>
                    <List.Item>
                      {t(
                        'game.content.ad.guide.container.flag_env',
                        'Never hard-code a platform-specific path. A creation-time RSCTF_FLAG compatibility value may exist, but it becomes stale after rotation — always read $RSCTF_FLAG_FILE.'
                      )}
                    </List.Item>
                    <List.Item>
                      {t(
                        'game.content.ad.guide.container.patch',
                        'Patch your service live inside the container — the platform does not redeploy on its own. Your changes live ONLY in the running container; there is no persistent disk.'
                      )}
                    </List.Item>
                    <List.Item>
                      {t(
                        'game.content.ad.guide.container.supervisor',
                        "Your service runs under a supervisor (PID 1). If the service process crashes — or you restart it — it comes back automatically and your patches stay. Restarting the service is safe; but don't kill PID 1 (the supervisor): that drops the box back to the base image."
                      )}
                    </List.Item>
                    <List.Item>
                      {t(
                        'game.content.ad.guide.container.reset',
                        'Reset rebuilds the box from the original image — it WIPES all your changes (patches included), costs SLA during the rebuild, and has a cooldown. Use it only if your box is wedged or compromised, then re-apply your patches.'
                      )}
                    </List.Item>
                  </List>
                </Accordion.Panel>
              </Accordion.Item>

              {/* DO / DON'T */}
              <Accordion.Item value="do_dont">
                <Accordion.Control icon={<Icon path={mdiRestart} size={1} color="var(--mantine-color-gray-6)" />}>
                  <Text fw={600}>{t('game.content.ad.guide.do_dont.title', "Do / Don't")}</Text>
                </Accordion.Control>
                <Accordion.Panel>
                  <Stack gap="xs">
                    <Text size="sm" fw={600} c="teal">
                      {t('game.content.ad.guide.do_dont.do', 'Do')}
                    </Text>
                    <List size="sm" spacing={2}>
                      <List.Item>
                        {t(
                          'game.content.ad.guide.do_dont.do_patch',
                          'Patch your service before the first attack lands.'
                        )}
                      </List.Item>
                      <List.Item>
                        {t(
                          'game.content.ad.guide.do_dont.do_automate',
                          'Automate flag submission — paste into a loop that fetches the current flag from each target and POSTs the batch.'
                        )}
                      </List.Item>
                      <List.Item>
                        {t(
                          'game.content.ad.guide.do_dont.do_monitor',
                          'Watch your check status badge — if it goes Mumble/Offline you are losing SLA every tick.'
                        )}
                      </List.Item>
                      <List.Item>
                        {t(
                          'game.content.ad.guide.do_dont.do_reapply',
                          'Re-apply your patches after any reset — a reset reverts to the pristine, vulnerable image.'
                        )}
                      </List.Item>
                    </List>
                    <Text size="sm" fw={600} c="red" mt="xs">
                      {t('game.content.ad.guide.do_dont.dont', "Don't")}
                    </Text>
                    <List size="sm" spacing={2}>
                      <List.Item>
                        {t(
                          'game.content.ad.guide.do_dont.dont_share',
                          'Share your flags or your token with other teams — operators detect it and apply penalties.'
                        )}
                      </List.Item>
                      <List.Item>
                        {t(
                          'game.content.ad.guide.do_dont.dont_dos',
                          "DoS another team's service or the checker infrastructure — instant DQ."
                        )}
                      </List.Item>
                      <List.Item>
                        {t(
                          'game.content.ad.guide.do_dont.dont_break',
                          'Patch in a way that breaks the legit check (returns 500 to the checker) — you lose SLA equivalent to being offline.'
                        )}
                      </List.Item>
                      <List.Item>
                        {t(
                          'game.content.ad.guide.do_dont.dont_hardcode',
                          "Hard-code the path or cache the flag — read $RSCTF_FLAG_FILE fresh each request, or you'll serve a stale flag and fail the check."
                        )}
                      </List.Item>
                      <List.Item>
                        {t(
                          'game.content.ad.guide.do_dont.dont_kill_pid1',
                          'Kill PID 1 (the supervisor) in your box — it resets you to the base image and wipes your patches.'
                        )}
                      </List.Item>
                    </List>
                  </Stack>
                </Accordion.Panel>
              </Accordion.Item>
            </Accordion>

            <Text size="xs" c="dimmed" ta="center">
              {t('game.content.ad.guide.footer', 'Endpoint base: ')}
              <Anchor href={apiUrl} target="_blank" rel="noreferrer" className={misc.ffmono}>
                {apiUrl}
              </Anchor>
            </Text>
          </Stack>
        </ScrollArea>
      </Modal>

      {/* Fresh-token reveal — shared with KotH (see AdToolkitSections). */}
      <AdTokenRevealModal
        opened={tokenModalOpen}
        onClose={closeTokenModal}
        freshToken={freshToken}
        title={t('game.content.ad.token_modal.title', 'Your new A&D API token')}
        warning={t(
          'game.content.ad.token_modal.warning',
          'This token is now saved in this browser (see “Saved token” in the API-token section) so your scripts can reuse it. Copy it here too if you want it elsewhere — the platform keeps only a hash and can’t show it again. The previous token (if any) has been invalidated.'
        )}
      />

      {/* Generated SSH keypair reveal — private key shown ONCE */}
      <Modal
        opened={privKeyModalOpen}
        size="lg"
        onClose={() => {
          closePrivKeyModal()
          setFreshPrivKey(null)
        }}
        title={t('game.content.ad.ssh_modal.title', 'Your new SSH keypair')}
        centered
      >
        <Stack gap="sm">
          <Alert color="orange" icon={<Icon path={mdiAlertCircleOutline} size={1} />}>
            {t(
              'game.content.ad.ssh_modal.warning',
              'Download the private key now — it will not be shown again. Save it somewhere ssh-agent can find it (e.g. ~/.ssh/) and chmod 600.'
            )}
          </Alert>
          <Text size="xs" fw={600}>
            {t('game.content.ad.ssh_modal.fingerprint', 'Fingerprint')}
          </Text>
          <Code className={misc.ffmono}>{freshPrivKey?.fingerprint}</Code>
          <Text size="xs" fw={600}>
            {t('game.content.ad.ssh_modal.private', 'Private key (PEM, OpenSSH format)')}
          </Text>
          <Box style={{ position: 'relative' }}>
            <Code block className={misc.ffmono} style={{ maxHeight: 220, overflow: 'auto', fontSize: '0.7rem' }}>
              {freshPrivKey?.privateKey}
            </Code>
          </Box>
          <Group justify="flex-end">
            <CopyButton value={freshPrivKey?.privateKey ?? ''}>
              {({ copied, copy }) => (
                <Button
                  variant="default"
                  leftSection={<Icon path={copied ? mdiCheck : mdiContentCopy} size={0.8} />}
                  onClick={copy}
                >
                  {copied ? t('game.tooltip.copy.copied', 'Copied') : t('game.button.ad.ssh.copy_private', 'Copy')}
                </Button>
              )}
            </CopyButton>
            <Button leftSection={<Icon path={mdiDownload} size={0.8} />} onClick={downloadPrivKey}>
              {t('game.button.ad.ssh.download_key', 'Download .key')}
            </Button>
            <Button
              onClick={() => {
                closePrivKeyModal()
                setFreshPrivKey(null)
              }}
            >
              {t('common.modal.confirm', 'Confirm')}
            </Button>
          </Group>
        </Stack>
      </Modal>
    </>
  )
}
