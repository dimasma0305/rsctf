import { Accordion, ActionIcon, Alert, Box, Button, Code, CopyButton, Group, Modal, Stack, Text, Tooltip } from '@mantine/core'
import { useDisclosure, useLocalStorage } from '@mantine/hooks'
import { mdiAlertCircleOutline, mdiCheck, mdiContentCopy, mdiDownload, mdiEye, mdiEyeOff, mdiKeyChain, mdiVpn } from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { showErrorMsg } from '@Utils/Shared'
import { useAdTokenHint } from '@Hooks/useGame'
import api, { AdTokenHintModel } from '@Api'
import misc from '@Styles/Misc.module.css'

/**
 * Shared token state + rotation flow for the A&D and KotH toolkits. The two
 * engines share one Bearer token (one string authenticates both /Submit and
 * /Koth/{id}/Token), so they share this hook rather than duplicating state.
 *
 * `freshToken` is kept in React state past the reveal-modal close so the
 * caller's curl examples can render with the real Bearer token for the rest
 * of the session; the DB only stores an HMAC hash, so it's gone on reload.
 *
 * `storedToken` persists the plaintext to this browser's localStorage (keyed
 * per game) so a player's bot/scripts can grab it later without re-rotating
 * (which would invalidate the token their bot is already using). It's the same
 * one string for both engines. This is a deliberate convenience/exposure
 * tradeoff — surfaced in the UI with a security note + a "Forget" control, and
 * a rotation overwrites it (the old value is invalid anyway).
 *
 * @param onRotated optional callback fired after a successful rotation — KotH
 *   uses it to show a success notification; A&D leaves it off.
 */
export const useAdToken = (gameId: number, onRotated?: () => void) => {
  const { t } = useTranslation()
  const { adTokenHint, mutate: mutateHint } = useAdTokenHint(gameId)

  const [rotating, setRotating] = useState(false)
  const [freshToken, setFreshToken] = useState<string | null>(null)
  // Per-game so switching games never surfaces the wrong token. JSON-serialized
  // by Mantine; null when nothing has been saved (or after Forget).
  // getInitialValueInEffect:false → read synchronously on first render (SPA, no
  // SSR) so the curl examples render with the saved token immediately instead of
  // flashing the <your-token> placeholder for a frame.
  const [storedToken, setStoredToken] = useLocalStorage<string | null>({
    key: `ad-api-token-${gameId}`,
    defaultValue: null,
    getInitialValueInEffect: false,
  })
  const [tokenModalOpen, { open: openTokenModal, close: closeTokenModal }] = useDisclosure(false)

  const onRotate = async () => {
    setRotating(true)
    try {
      const { data } = await api.game.gameAdRotateToken(gameId)
      setFreshToken(data.token)
      setStoredToken(data.token) // persist for bot/script reuse across reloads
      openTokenModal()
      mutateHint()
      onRotated?.()
    } catch (e) {
      showErrorMsg(e, t)
    } finally {
      setRotating(false)
    }
  }

  const forgetToken = () => setStoredToken(null)

  return { adTokenHint, rotating, freshToken, storedToken, forgetToken, tokenModalOpen, closeTokenModal, onRotate }
}

interface AdTokenSectionProps {
  hint?: AdTokenHintModel
  rotating: boolean
  onRotate: () => void
  /** Section title — engine-specific copy (A&D vs KotH namespace). */
  title: string
  /** Section intro — wording differs per engine. */
  intro: string
  /** "Your current token" label — engine-specific copy. */
  currentLabel: string
  /** Plaintext token persisted in this browser (from useAdToken.storedToken). */
  storedToken?: string | null
  /** Clear the persisted token (from useAdToken.forgetToken). */
  onForget?: () => void
}

/** Mask a token to prefix + last 4 so it can be shown without fully revealing. */
const maskToken = (tok: string) =>
  tok.length <= 12 ? tok : `${tok.slice(0, 7)}${'•'.repeat(6)}${tok.slice(-4)}`

/**
 * The "Your API token" accordion item, shared by the A&D and KotH toolkits.
 * Renders the current-token hint + rotate/generate button + last-used line, and
 * — when a token has been saved to this browser — a reveal/copy/forget block so
 * a player's bot can reuse the same string across reloads.
 * Must be rendered inside a Mantine <Accordion> (it returns an Accordion.Item).
 */
export const AdTokenSection: FC<AdTokenSectionProps> = ({
  hint,
  rotating,
  onRotate,
  title,
  intro,
  currentLabel,
  storedToken,
  onForget,
}) => {
  const { t } = useTranslation()
  const [revealed, setRevealed] = useState(false)

  return (
    <Accordion.Item value="token">
      <Accordion.Control icon={<Icon path={mdiKeyChain} size={1} color="var(--mantine-color-orange-6)" />}>
        <Text fw={600}>{title}</Text>
      </Accordion.Control>
      <Accordion.Panel>
        <Stack gap="sm">
          <Text size="sm">{intro}</Text>
          <Group justify="space-between" wrap="wrap" gap="xs">
            <Group gap="xs">
              <Text size="sm" fw={600}>
                {currentLabel}:
              </Text>
              {hint?.exists ? (
                <Text size="sm" className={misc.ffmono}>
                  {hint.hint}
                </Text>
              ) : (
                <Text size="sm" c="dimmed">
                  {t('game.content.ad.no_token_yet', 'No token yet')}
                </Text>
              )}
            </Group>
            <Button
              size="xs"
              variant="default"
              leftSection={<Icon path={mdiKeyChain} size={0.7} />}
              loading={rotating}
              onClick={onRotate}
            >
              {hint?.exists
                ? t('game.button.ad.rotate_token', 'Rotate token')
                : t('game.button.ad.generate_token', 'Generate token')}
            </Button>
          </Group>
          {hint?.exists && (
            <Text size="xs" c="dimmed">
              {t('game.content.ad.last_used', 'Last used')}:{' '}
              {hint.lastUsedAt
                ? dayjs(hint.lastUsedAt).fromNow()
                : t('game.content.ad.never_used', 'never')}
            </Text>
          )}

          {/* Saved-token block — present only after a rotation has persisted the
              plaintext to this browser, so a bot/script can grab it later. */}
          {storedToken ? (
            <Stack gap={4}>
              <Group justify="space-between" wrap="nowrap" gap="xs" align="center">
                <Group gap="xs" wrap="nowrap" style={{ minWidth: 0, flex: 1 }}>
                  <Text size="sm" fw={600} style={{ whiteSpace: 'nowrap' }}>
                    {t('game.content.ad.saved_token', 'Saved token')}:
                  </Text>
                  <Code className={misc.ffmono} style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                    {revealed ? storedToken : maskToken(storedToken)}
                  </Code>
                  <Tooltip label={revealed ? t('game.button.ad.hide_token', 'Hide') : t('game.button.ad.reveal_token', 'Reveal')} withArrow>
                    <ActionIcon
                      variant="subtle"
                      size="sm"
                      onClick={() => setRevealed((v) => !v)}
                      aria-label={revealed ? t('game.button.ad.hide_token', 'Hide token') : t('game.button.ad.reveal_token', 'Reveal token')}
                    >
                      <Icon path={revealed ? mdiEyeOff : mdiEye} size={0.7} />
                    </ActionIcon>
                  </Tooltip>
                </Group>
                <Group gap={4} wrap="nowrap">
                  <CopyButton value={storedToken}>
                    {({ copied, copy }) => (
                      <Button
                        size="compact-xs"
                        variant="light"
                        leftSection={<Icon path={copied ? mdiCheck : mdiContentCopy} size={0.7} />}
                        onClick={copy}
                      >
                        {copied ? t('game.tooltip.copy.copied', 'Copied') : t('game.button.ad.copy_token', 'Copy token')}
                      </Button>
                    )}
                  </CopyButton>
                  {onForget && (
                    <Button size="compact-xs" variant="subtle" color="red" onClick={onForget}>
                      {t('game.button.ad.forget_token', 'Forget')}
                    </Button>
                  )}
                </Group>
              </Group>
              <Text size="xs" c="dimmed">
                {t(
                  'game.content.ad.saved_token_note',
                  'Saved in THIS browser so your bot/scripts can reuse it — it survives reloads. Anyone with access to this browser can read it. “Rotate” issues a new token (invalidating this one); “Forget” removes it from this browser.'
                )}
              </Text>
            </Stack>
          ) : (
            <Text size="xs" c="dimmed">
              {t('game.content.ad.saved_token_hint', 'Generate or rotate a token and it’s saved in this browser so your bot/scripts can reuse it later.')}
            </Text>
          )}
        </Stack>
      </Accordion.Panel>
    </Accordion.Item>
  )
}

interface AdVpnSectionProps {
  gameId: number
  /** Section title — engine-specific copy. */
  title: string
  /** Section intro — wording differs per engine. */
  intro: string
  /** Platform setup hint shown under the download button. */
  linuxHint: string
}

/**
 * The "VPN config" accordion item, shared by the A&D and KotH toolkits. One
 * WireGuard tunnel reaches both engines' bridges, so both link to the same
 * /Ad/Vpn/Config endpoint. Must be rendered inside a Mantine <Accordion>.
 */
export const AdVpnSection: FC<AdVpnSectionProps> = ({ gameId, title, intro, linuxHint }) => {
  const { t } = useTranslation()

  return (
    <Accordion.Item value="vpn">
      <Accordion.Control icon={<Icon path={mdiVpn} size={1} color="var(--mantine-color-cyan-6)" />}>
        <Text fw={600}>{title}</Text>
      </Accordion.Control>
      <Accordion.Panel>
        <Stack gap="sm">
          <Text size="sm">{intro}</Text>
          <Group gap="sm">
            <Button
              leftSection={<Icon path={mdiDownload} size={0.9} />}
              component="a"
              href={`/api/Game/${gameId}/Ad/Vpn/Config`}
              download
            >
              {t('game.button.ad.download_vpn', 'Download .conf')}
            </Button>
          </Group>
          <Text size="xs" c="dimmed">
            {linuxHint}
          </Text>
        </Stack>
      </Accordion.Panel>
    </Accordion.Item>
  )
}

interface AdTokenRevealModalProps {
  opened: boolean
  onClose: () => void
  freshToken: string | null
  /** Modal title — engine-specific copy. */
  title: string
  /** Save-it-now warning — engine-specific copy. */
  warning: string
}

/**
 * Fresh-token reveal modal shared by the A&D and KotH toolkits — shows the
 * plaintext token exactly once after a rotation.
 */
export const AdTokenRevealModal: FC<AdTokenRevealModalProps> = ({
  opened,
  onClose,
  freshToken,
  title,
  warning,
}) => {
  const { t } = useTranslation()

  return (
    <Modal opened={opened} onClose={onClose} title={title} centered>
      <Stack gap="sm">
        <Alert color="orange" icon={<Icon path={mdiAlertCircleOutline} size={1} />}>
          {warning}
        </Alert>
        <Box style={{ position: 'relative' }}>
          <Code block className={misc.ffmono}>
            {freshToken}
          </Code>
        </Box>
        <Group justify="flex-end">
          <CopyButton value={freshToken ?? ''}>
            {({ copied, copy }) => (
              <Button
                variant="default"
                leftSection={<Icon path={copied ? mdiCheck : mdiContentCopy} size={0.8} />}
                onClick={copy}
              >
                {copied ? t('game.tooltip.copy.copied', 'Copied') : t('game.button.ad.copy_token', 'Copy token')}
              </Button>
            )}
          </CopyButton>
          <Button onClick={onClose}>{t('common.modal.confirm', 'Confirm')}</Button>
        </Group>
      </Stack>
    </Modal>
  )
}
