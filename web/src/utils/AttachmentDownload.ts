export interface AttachmentDownloadInfo {
  isLocal: boolean
  filename: string | null
  sha256: string | null
}

const SHA256 = /^[0-9a-f]{64}$/i

const safeDecode = (value: string) => {
  try {
    return decodeURIComponent(value)
  } catch {
    return value
  }
}

/**
 * Extract immutable attachment metadata from RSCTF's content-addressed URL.
 * API-provided metadata wins; URL parsing is only a compatibility fallback for
 * older servers and therefore accepts exactly the two supported asset routes.
 */
export const attachmentDownloadInfo = (url?: string | null, apiSha256?: string | null): AttachmentDownloadInfo => {
  if (!url) return { isLocal: false, filename: null, sha256: null }

  const path = url.split(/[?#]/, 1)[0]
  const segments = path.split('/').filter(Boolean)
  const isPlainAsset = segments.length === 3 && segments[0] === 'assets'
  const isTokenAsset = segments.length === 5 && segments[0] === 'assets' && segments[2] === 's'
  const isLocal = isPlainAsset || isTokenAsset
  if (!isLocal) return { isLocal: false, filename: null, sha256: null }

  const urlHash = SHA256.test(segments[1] ?? '') ? segments[1].toLowerCase() : null
  const suppliedHash = apiSha256 && SHA256.test(apiSha256) ? apiSha256.toLowerCase() : null

  return {
    isLocal: true,
    filename: safeDecode(segments.at(-1) ?? '') || null,
    sha256: suppliedHash ?? urlHash,
  }
}

export const abbreviatedSha256 = (sha256: string) => `${sha256.slice(0, 12)}…${sha256.slice(-8)}`

/**
 * How the attachment button must behave.
 * - `custom`: the host supplies the download (editor preview).
 * - `external`: an off-origin link opens in a new tab.
 * - `direct`: a plain resumable `<a download>` to the local asset route.
 * - `granted`: same URL, but the event requires VPN proof, so a short-lived
 *   download grant is minted through the proof-aware client first. The grant
 *   travels as a path-scoped HttpOnly cookie; the URL itself never changes.
 */
export type AttachmentDownloadMode = 'custom' | 'external' | 'direct' | 'granted'

export const attachmentDownloadMode = (
  info: AttachmentDownloadInfo,
  options: { hasCustomDownload?: boolean; eventVpnRequired?: boolean; gameId?: number }
): AttachmentDownloadMode => {
  if (options.hasCustomDownload) return 'custom'
  if (!info.isLocal) return 'external'
  const gameId = options.gameId
  if (options.eventVpnRequired && info.sha256 && typeof gameId === 'number' && Number.isSafeInteger(gameId) && gameId > 0) {
    return 'granted'
  }
  return 'direct'
}
