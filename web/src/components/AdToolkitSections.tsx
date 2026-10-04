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
import {
  claimPlayerCredentialOperation,
  clearPlayerCredentialOperation,
  ownsPlayerCredentialResult,
  parsePlayerCredentialRevision,
  playerCredentialOperationStorageKey,
  playerCredentialOperationWasRejected,
  playerCredentialRevisionSignalKey,
  playerCredentialStorage,
  publishPlayerCredentialRevision,
  withPlayerCredentialLock,
} from '@Utils/PlayerCredentialOperations'
import { showErrorMsg } from '@Utils/Shared'
import { useViewerIdentity } from '@Utils/ViewerIdentity'
import { useAdTokenHint } from '@Hooks/useGame'
import api, { AdTokenHintModel } from '@Api'
import misc from '@Styles/Misc.module.css'

type TokenRevealSource = 'ad' | 'koth'

interface TokenMutationResult {
  token: string
  operationId: string
  revision: number
  participationId: number
  teamId: number
}

const tokenRequests = new Map<string, Promise<TokenMutationResult>>()

/**
 * Shared token state + rotation flow for the A&D and KotH toolkits. The two
 * engines share one Bearer token (one string authenticates both /Submit and
 * /Koth/{id}/Token), so they share this hook rather than duplicating state.
 *
 * `freshToken` is kept in React state past the reveal-modal close so the
 * caller's curl examples can render with the real Bearer token for the rest
 * of the session; the DB only stores an HMAC hash, so it's gone on reload.
 *
 * `storedToken` is session-memory only. It is cleared on account/game changes
 * and disappears on reload, so a shared browser cannot reveal another
 * account's still-valid team credential after logout.
 *
 * This hook is mounted once by the game page and shared by both toolkit modals.
 * The module request owner is a second fence for unusual duplicate mounts.
 */
export const useAdToken = (gameId: number, doFetch: boolean = true) => {
  const { t } = useTranslation()
  const { scope } = useViewerIdentity()
  const { adTokenHint, mutate: mutateHint } = useAdTokenHint(gameId, doFetch)

  const [rotating, setRotating] = useState(false)
  const [freshToken, setFreshToken] = useState<string | null>(null)
  const [storedToken, setStoredToken] = useState<string | null>(null)
  const [tokenRevision, setTokenRevision] = useState<number | null>(null)
  const rotatingRef = useRef(false)
  const responseGeneration = useRef(0)
  const revealedCredentialScope = useRef<{ participationId: number; teamId: number } | null>(null)
  const [revealSource, setRevealSource] = useState<TokenRevealSource | null>(null)
  const [tokenModalOpen, { open: openTokenModal, close: closeTokenModal }] = useDisclosure(false)

  useEffect(() => {
    rotatingRef.current = false
    responseGeneration.current += 1
    setRotating(false)
    setFreshToken(null)
    setStoredToken(null)
    setTokenRevision(null)
    revealedCredentialScope.current = null
    setRevealSource(null)
    closeTokenModal()
  }, [closeTokenModal, gameId, scope])

  useEffect(() => {
    const revealed = revealedCredentialScope.current
    if (
      !revealed ||
      adTokenHint?.participationId === undefined ||
      adTokenHint?.teamId === undefined ||
      (revealed.participationId === adTokenHint.participationId && revealed.teamId === adTokenHint.teamId)
    ) {
      return
    }
    responseGeneration.current += 1
    revealedCredentialScope.current = null
    setFreshToken(null)
    setStoredToken(null)
    setTokenRevision(null)
    setRevealSource(null)
    closeTokenModal()
  }, [adTokenHint?.participationId, adTokenHint?.teamId, closeTokenModal])

  useEffect(() => {
    if (typeof window === 'undefined') return
    const signalKey = playerCredentialRevisionSignalKey(gameId, 'ad-token')
    const onStorage = (event: StorageEvent) => {
      if (event.key !== signalKey) return
      const signal = parsePlayerCredentialRevision(event.newValue)
      if (!signal || (tokenRevision !== null && signal.revision <= tokenRevision)) return
      responseGeneration.current += 1
      setFreshToken(null)
      setStoredToken(null)
      setTokenRevision(null)
      revealedCredentialScope.current = null
      setRevealSource(null)
      closeTokenModal()
      void mutateHint()
    }
    window.addEventListener('storage', onStorage)
    return () => window.removeEventListener('storage', onStorage)
  }, [closeTokenModal, gameId, mutateHint, tokenRevision])

  const onRotate = async (source: TokenRevealSource = 'ad') => {
    if (rotatingRef.current) return false
    rotatingRef.current = true
    setRotating(true)
    const generation = ++responseGeneration.current
    const storage = playerCredentialStorage()
    const operationKey = playerCredentialOperationStorageKey(scope, gameId, 'ad-token')
    try {
      let request = tokenRequests.get(operationKey)
      if (!request) {
        request = withPlayerCredentialLock(operationKey, async () => {
          const operation = claimPlayerCredentialOperation(storage, operationKey, adTokenHint?.revision ?? 0, 'rotate')
          try {
            const { data } = await api.game.gameAdRotateToken(gameId, {
              operationId: operation.operationId,
              expectedRevision: operation.expectedRevision,
            })
            if (!ownsPlayerCredentialResult(storage, operationKey, operation, data)) {
              throw new Error('A stale credential response was ignored')
            }
            if (
              adTokenHint &&
              (data.participationId !== adTokenHint.participationId || data.teamId !== adTokenHint.teamId)
            ) {
              throw new Error('A credential response for an older team was ignored')
            }
            clearPlayerCredentialOperation(storage, operationKey, operation.operationId)
            publishPlayerCredentialRevision(storage, playerCredentialRevisionSignalKey(gameId, 'ad-token'), {
              operationId: data.operationId,
              revision: data.revision,
            })
            return {
              token: data.token,
              operationId: data.operationId,
              revision: data.revision,
              participationId: data.participationId,
              teamId: data.teamId,
            }
          } catch (error) {
            if (playerCredentialOperationWasRejected(error)) {
              clearPlayerCredentialOperation(storage, operationKey, operation.operationId)
            }
            throw error
          }
        })
        tokenRequests.set(operationKey, request)
        const cleanup = () => {
          if (tokenRequests.get(operationKey) === request) tokenRequests.delete(operationKey)
        }
        void request.then(cleanup, cleanup)
      }
      const result = await request
      if (generation !== responseGeneration.current) return false
      setFreshToken(result.token)
      setStoredToken(result.token)
      setTokenRevision(result.revision)
      revealedCredentialScope.current = {
        participationId: result.participationId,
        teamId: result.teamId,
      }
      setRevealSource(source)
      openTokenModal()
      await mutateHint()
      return true
    } catch (e) {
      await mutateHint().catch(() => undefined)
      showErrorMsg(e, t)
      return false
    } finally {
      rotatingRef.current = false
      setRotating(false)
    }
  }

  const forgetToken = () => {
    responseGeneration.current += 1
    revealedCredentialScope.current = null
    setFreshToken(null)
    setStoredToken(null)
    setTokenRevision(null)
    setRevealSource(null)
    closeTokenModal()
  }

  return {
    adTokenHint,
    rotating,
    freshToken,
    storedToken,
    forgetToken,
    tokenModalOpen,
    closeTokenModal,
    onRotate,
    revealSource,
  }
}

export type AdTokenOwner = ReturnType<typeof useAdToken>

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
  /** Plaintext token retained only in this mounted page session. */
  storedToken?: string | null
  /** Clear the session-memory token (from useAdToken.forgetToken). */
  onForget?: () => void
}

/** Mask a token to prefix + last 4 so it can be shown without fully revealing. */
const maskToken = (tok: string) => (tok.length <= 12 ? tok : `${tok.slice(0, 7)}${'•'.repeat(6)}${tok.slice(-4)}`)

/**
 * The "Your API token" accordion item, shared by the A&D and KotH toolkits.
 * Renders the current-token hint + rotate/generate button + last-used line, and
 * — when a token has been generated in this page session — a reveal/copy/forget
 * block so a player can reuse the same string in command examples.
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

          {/* Session-token block — present only after this mounted page owns a
              successful rotation response. No plaintext enters web storage. */}
          {storedToken ? (
            <Stack gap={4}>
              <Group justify="space-between" wrap="nowrap" gap="xs" align="center">
                <Group gap="xs" wrap="nowrap" style={{ minWidth: 0, flex: 1 }}>
                  <Text size="sm" fw={600} style={{ whiteSpace: 'nowrap' }}>
                    {t('game.content.ad.saved_token', 'Saved token')}:
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
                  'Kept only in this page session for your command examples. It is cleared on reload, logout, or account/game changes. “Rotate” invalidates the previous token; “Forget” clears this copy now.'
                )}
              </Text>
            </Stack>
          ) : (
            <Text size="xs" c="dimmed">
              {t(
                'game.content.ad.saved_token_hint',
                'Generate or rotate a token to use it in command examples for this page session.'
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
 * WireGuard tunnel reaches both engines' bridges. When the event VPN gate is
 * enabled, this endpoint returns the same personal profile as the event-page
 * download; otherwise it preserves the team profile used by legacy A&D events.
 * Must be rendered inside a Mantine <Accordion>.
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
