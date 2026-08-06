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
