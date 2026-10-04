import { allowEventVpnReconnectRetry, eventVpnAccessErrorMessage, isEventVpnAccessError } from '@Utils/EventVpnProof'
import { httpErrorStatus } from '@Utils/HttpError'
import api from '@Api'

type Translate = (key: string, fallback: string) => string

/** Navigate a browser-native download of a same-origin asset URL. The URL is
 * unchanged, so `Content-Disposition`, ETag caching, and range resumption all
 * keep working; only the grant cookie set moments earlier is new. */
export const startAttachmentDownload = (href: string, filename?: string | null) => {
  const anchor = document.createElement('a')
  anchor.href = href
  anchor.download = filename ?? ''
  document.body.appendChild(anchor)
  try {
    anchor.click()
  } finally {
    anchor.remove()
  }
}

/** Mint the download grant through the proof-instrumented client, then start
 * the download. A click is an explicit user retry, so a previously observed
 * disconnect no longer blocks one fresh proof attempt. */
export const downloadGrantedAttachment = async (
  gameId: number,
  hash: string,
  href: string,
  filename?: string | null,
  deps: {
    grant?: (gameId: number, hash: string) => Promise<unknown>
    start?: (href: string, filename?: string | null) => void
  } = {}
) => {
  allowEventVpnReconnectRetry(gameId)
  const grant = deps.grant ?? ((game, asset) => api.eventSecurity.gameAssetGrant(game, asset))
  await grant(gameId, hash)
  ;(deps.start ?? startAttachmentDownload)(href, filename)
}

/** Distinct, textual failure copy for a grant that could not be minted. VPN
 * failures reuse the challenge-polling copy so the two surfaces agree. */
export const attachmentGrantErrorMessage = (error: unknown, t: Translate) => {
  if (isEventVpnAccessError(error)) return eventVpnAccessErrorMessage(error, t)
  const status = httpErrorStatus(error)
  if (status === 401) {
    return t('challenge.error.unauthorized', 'Your session expired. Sign in again to continue.')
  }
  if (status === 403) {
    return t(
      'challenge.error.forbidden',
      'Your challenge access was revoked or is no longer valid. Rejoin the event or ask an organizer to check your participation.'
    )
  }
  if (status === 429) {
    return t(
      'challenge.error.vpn_rate_limited',
      'Event VPN verification is rate limited. Retry after the indicated delay.'
    )
  }
  return t('challenge.error.attachment_grant', 'The attachment download could not be authorized. Retry the download.')
}
