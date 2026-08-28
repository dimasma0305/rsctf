export const LEGACY_AD_TOKEN_PREFIX = 'ad-api-token-'

/**
 * Remove plaintext bearer tokens written by releases before v0.1.96.
 * This is synchronous and local so logout cleanup cannot depend on the
 * network request succeeding.
 */
export const clearLegacyAdTokenStorage = (storage?: Pick<Storage, 'key' | 'length' | 'removeItem'>) => {
  const target = storage ?? (typeof window === 'undefined' ? undefined : window.localStorage)
  if (!target) return 0
  const stale: string[] = []
  try {
    for (let index = 0; index < target.length; index += 1) {
      const key = target.key(index)
      if (key?.startsWith(LEGACY_AD_TOKEN_PREFIX)) stale.push(key)
    }
    for (const key of stale) target.removeItem(key)
  } catch {
    // Storage access can be denied. New code never writes the bearer there.
    return 0
  }
  return stale.length
}
