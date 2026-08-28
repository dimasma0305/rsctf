import {
  Accordion,
  ActionIcon,
  Alert,
  Box,
  Button,
  Code,
  CopyButton,
  Group,
  Modal,
  Stack,
  Text,
  Tooltip,
} from '@mantine/core'
import { useDisclosure } from '@mantine/hooks'
import {
  mdiAlertCircleOutline,
  mdiCheck,
  mdiContentCopy,
  mdiDownload,
  mdiEye,
  mdiEyeOff,
  mdiKeyChain,
  mdiVpn,
} from '@mdi/js'
import { Icon } from '@mdi/react'
import dayjs from 'dayjs'
import { FC, useEffect, useRef, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { clearLegacyAdTokenStorage } from '@Utils/AdTokenMemory'
import { adTokenViewerScope, isCurrentAdTokenViewer } from '@Utils/AdTokenScope'
import {
  claimPlayerCredentialOperation,
  clearPlayerCredentialOperation,
  ownsPlayerCredentialResult,
  playerCredentialOperationStorageKey,
  PlayerCredentialOperation,
} from '@Utils/PlayerCredentialOperations'
import { showErrorMsg } from '@Utils/Shared'
import { useAdTokenHint } from '@Hooks/useGame'
import { useUser } from '@Hooks/useUser'
import api, { AdTokenHintModel } from '@Api'
import misc from '@Styles/Misc.module.css'

const adTokenRequests = new Map<
  number,
  Promise<{
    token: string
    revision: number
    participationId: number
    teamId: number
    operation: PlayerCredentialOperation
  }>
>()

const claimAdTokenOperation = async (gameId: number, revision: number) => {
  const key = playerCredentialOperationStorageKey(gameId, 'ad-token')
  const claim = () => claimPlayerCredentialOperation(window.localStorage, key, revision)
  if (typeof navigator !== 'undefined' && navigator.locks) {
    return navigator.locks.request(`rsctf:${key}`, claim)
  }
  return claim()
}

const rotateAdTokenOnce = (
  gameId: number,
  revision: number
): Promise<{
  token: string
  revision: number
  participationId: number
  teamId: number
  operation: PlayerCredentialOperation
}> => {
  const active = adTokenRequests.get(gameId)
  if (active) return active
  const request = (async () => {
    const key = playerCredentialOperationStorageKey(gameId, 'ad-token')
    const operation = await claimAdTokenOperation(gameId, revision)
    try {
      const { data } = await api.game.gameAdRotateToken(gameId, {
        operationId: operation.operationId,
        expectedRevision: operation.expectedRevision,
      })
      if (!ownsPlayerCredentialResult(window.localStorage, key, operation, data)) {
        throw new Error('A stale credential response was ignored')
      }
      clearPlayerCredentialOperation(window.localStorage, key, operation.operationId)
      return {
        token: data.token,
        revision: data.revision,
        participationId: data.participationId,
        teamId: data.teamId,
        operation,
      }
    } catch (error) {
      if ((error as { response?: { status?: number } })?.response?.status === 409) {
        clearPlayerCredentialOperation(window.localStorage, key, operation.operationId)
      }
      throw error
    }
  })()
  adTokenRequests.set(gameId, request)
  const cleanup = () => {
    if (adTokenRequests.get(gameId) === request) adTokenRequests.delete(gameId)
  }
  void request.then(cleanup, cleanup)
  return request
}

/**
 * Shared token state + rotation flow for the A&D and KotH toolkits. The two
 * engines share one Bearer token (one string authenticates both /Submit and
 * /Koth/{id}/Token), so they share this hook rather than duplicating state.
 *
 * `freshToken` is kept in React state past the reveal-modal close so the
 * caller's curl examples can render with the real Bearer token for the rest
 * of the session; the DB only stores an HMAC hash, so it's gone on reload.
 *
 * The plaintext exists only in this mounted browser session. The database
 * stores an HMAC and browser storage is deliberately never used, so a reload,
 * logout, account replacement or participation change cannot reveal a previous
 * player's bearer on a shared device.
 *
 * @param onRotated optional callback fired after a successful rotation — KotH
 *   uses it to show a success notification; A&D leaves it off.
 */
export const useAdToken = (gameId: number, onRotated?: () => void, enabled: boolean = true) => {
  const { t } = useTranslation()
  const { user } = useUser()
  const { adTokenHint, mutate: mutateHint } = useAdTokenHint(gameId, enabled)
  const currentScope = adTokenViewerScope(adTokenHint)
  const currentScopeRef = useRef(currentScope)
  currentScopeRef.current = currentScope

  const [rotating, setRotating] = useState(false)
  const [freshToken, setFreshToken] = useState<string | null>(null)
  const [tokenModalOpen, { open: openTokenModal, close: closeTokenModal }] = useDisclosure(false)

  useEffect(() => {
    clearLegacyAdTokenStorage()
    setFreshToken(null)
    closeTokenModal()
  }, [gameId, user?.userId, adTokenHint?.participationId, adTokenHint?.teamId, closeTokenModal])

  const onRotate = async () => {
    if (!adTokenHint) return
    setRotating(true)
    try {
      const requestedScope = adTokenViewerScope(adTokenHint)
      const { token, participationId, teamId } = await rotateAdTokenOnce(gameId, adTokenHint?.revision ?? 0)
      if (
        !isCurrentAdTokenViewer(
          requestedScope,
          { participationId, teamId },
          currentScopeRef.current
        )
      ) {
        throw new Error('A credential response for a previous participation was ignored')
      }
      setFreshToken(token)
      openTokenModal()
      await mutateHint()
      onRotated?.()
    } catch (e) {
      await mutateHint().catch(() => undefined)
      showErrorMsg(e, t)
    } finally {
      setRotating(false)
    }
  }

  const forgetToken = () => setFreshToken(null)

  return {
    adTokenHint,
    rotating,
    freshToken,
    storedToken: freshToken,
    forgetToken,
    tokenModalOpen,
    closeTokenModal,
    onRotate,
  }
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
  /** Plaintext token held only by the mounted toolkit session. */
  storedToken?: string | null
  /** Clear the in-memory token (from useAdToken.forgetToken). */
  onForget?: () => void
}

/** Mask a token to prefix + last 4 so it can be shown without fully revealing. */
const maskToken = (tok: string) => (tok.length <= 12 ? tok : `${tok.slice(0, 7)}${'•'.repeat(6)}${tok.slice(-4)}`)

/**
 * The "Your API token" accordion item, shared by the A&D and KotH toolkits.
 * Renders the current-token hint + rotate/generate button + last-used line, and
 * — while the freshly rotated token remains in this mounted session — a
 * reveal/copy/forget block. Plaintext never persists across a reload.
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
              {hint.lastUsedAt ? dayjs(hint.lastUsedAt).fromNow() : t('game.content.ad.never_used', 'never')}
            </Text>
          )}

          {/* Session-token block — the plaintext is never put in browser storage. */}
          {storedToken ? (
            <Stack gap={4}>
              <Group justify="space-between" wrap="nowrap" gap="xs" align="center">
                <Group gap="xs" wrap="nowrap" style={{ minWidth: 0, flex: 1 }}>
                  <Text size="sm" fw={600} style={{ whiteSpace: 'nowrap' }}>
                    {t('game.content.ad.saved_token', 'Current session token')}:
                  </Text>
                  <Code
                    className={misc.ffmono}
                    style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}
                  >
                    {revealed ? storedToken : maskToken(storedToken)}
                  </Code>
                  <Tooltip
                    label={
                      revealed ? t('game.button.ad.hide_token', 'Hide') : t('game.button.ad.reveal_token', 'Reveal')
                    }
                    withArrow
                  >
                    <ActionIcon
                      variant="subtle"
                      size="sm"
                      onClick={() => setRevealed((v) => !v)}
                      aria-label={
                        revealed
                          ? t('game.button.ad.hide_token', 'Hide token')
                          : t('game.button.ad.reveal_token', 'Reveal token')
                      }
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
                        {copied
                          ? t('game.tooltip.copy.copied', 'Copied')
                          : t('game.button.ad.copy_token', 'Copy token')}
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
                  'Available only in this tab until you reload, log out, change account, or choose Forget. Copy or download it for your bot now. Rotate invalidates the previous token.'
                )}
              </Text>
            </Stack>
          ) : (
            <Text size="xs" c="dimmed">
                {t(
                  'game.content.ad.saved_token_hint',
                  'Generate or rotate a token, then copy it for your bot. The platform will not store the plaintext in this browser.'
                )}
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
export const AdTokenRevealModal: FC<AdTokenRevealModalProps> = ({ opened, onClose, freshToken, title, warning }) => {
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
