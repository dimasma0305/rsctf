/**
 * Client mirror of the server's AI chat share-link rules, used only for live
 * feedback. The server stays authoritative for acceptance and matching.
 */

export const AI_CHAT_URL_MAX_LENGTH = 2048
export const AI_CHAT_PROVIDER_KEY_PATTERN = /^[a-z0-9][a-z0-9-]{0,39}$/
export const AI_CHAT_PROVIDER_LABEL_MAX_LENGTH = 64
export const AI_CHAT_PROVIDER_PATTERN_MAX_LENGTH = 512

export type AiChatUrlInvalidReason = 'empty' | 'invalid' | 'https_required' | 'credentials' | 'port' | 'too_long'

export type AiChatUrlResult = { ok: true; url: string } | { ok: false; reason: AiChatUrlInvalidReason }

export interface AiChatProviderPattern {
  key: string
  label: string
  pattern: string
}

/**
 * Normalize a pasted share link: absolute https URL, no userinfo, default
 * port, fragment removed. The result equals `URL.href` after clearing the
 * hash, so a bare trailing `?` is kept exactly as the server keeps it.
 */
export const normalizeAiChatUrl = (input: string): AiChatUrlResult => {
  const trimmed = input.trim()
  if (!trimmed) return { ok: false, reason: 'empty' }

  let parsed: URL
  try {
    parsed = new URL(trimmed)
  } catch {
    return { ok: false, reason: 'invalid' }
  }

  if (parsed.protocol !== 'https:') return { ok: false, reason: 'https_required' }
  if (parsed.username || parsed.password) return { ok: false, reason: 'credentials' }
  if (parsed.port) return { ok: false, reason: 'port' }
  if (!parsed.hostname) return { ok: false, reason: 'invalid' }

  parsed.hash = ''
  const url = parsed.href
  if (url.length > AI_CHAT_URL_MAX_LENGTH) return { ok: false, reason: 'too_long' }
  return { ok: true, url }
}

const MAX_COMPILED_PATTERNS = 64
const compiledPatterns = new Map<string, RegExp | null>()

/** Compile `^(?:pattern)$` with the `u` flag; invalid patterns compile to null. */
export const compileAiChatPattern = (pattern: string): RegExp | null => {
  const cached = compiledPatterns.get(pattern)
  if (cached !== undefined) return cached

  let compiled: RegExp | null
  try {
    compiled = new RegExp(`^(?:${pattern})$`, 'u')
  } catch {
    compiled = null
  }
  if (compiledPatterns.size >= MAX_COMPILED_PATTERNS) {
    const oldest = compiledPatterns.keys().next().value
    if (oldest !== undefined) compiledPatterns.delete(oldest)
  }
  compiledPatterns.set(pattern, compiled)
  return compiled
}

/** First provider (in the given order) whose pattern matches a normalized URL. */
export const matchAiChatProvider = <T extends AiChatProviderPattern>(
  url: string,
  providers: readonly T[]
): T | null => {
  for (const provider of providers) {
    if (compileAiChatPattern(provider.pattern)?.test(url)) return provider
  }
  return null
}

/** Server match order: enabled built-ins as listed, then enabled custom providers by key. */
export const aiChatMatchOrder = <T extends AiChatProviderPattern & { builtin: boolean; enabled: boolean }>(
  providers: readonly T[]
): T[] => {
  const enabled = providers.filter((provider) => provider.enabled)
  const custom = enabled
    .filter((provider) => !provider.builtin)
    .sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0))
  return [...enabled.filter((provider) => provider.builtin), ...custom]
}

/** A link is only rendered as a clickable anchor when it still normalizes. */
export const safeAiChatHref = (url: string): string | null => {
  const normalized = normalizeAiChatUrl(url)
  return normalized.ok ? normalized.url : null
}

export const aiChatHostname = (url: string): string => {
  try {
    return new URL(url).hostname
  } catch {
    return ''
  }
}

export type AiChatPatternInvalidReason = 'empty' | 'too_long' | 'https_prefix' | 'alternation' | 'syntax'

/** An unescaped `|` outside every group or class escapes the site-bound prefix (mirrors the server). */
export const hasTopLevelAlternation = (pattern: string): boolean => {
  let depth = 0
  let inClass = false
  let escaped = false
  for (const ch of pattern) {
    if (escaped) {
      escaped = false
      continue
    }
    if (ch === '\\') escaped = true
    else if (ch === '[' && !inClass) inClass = true
    else if (ch === ']' && inClass) inClass = false
    else if (ch === '(' && !inClass) depth++
    else if (ch === ')' && !inClass) depth = Math.max(0, depth - 1)
    else if (ch === '|' && !inClass && depth === 0) return true
  }
  return false
}

/** Admin-form check: https:// prefix, bounded length, compiles as a `u` regex bare and anchored. */
export const validateAiChatPattern = (pattern: string): AiChatPatternInvalidReason | null => {
  if (!pattern) return 'empty'
  if (pattern.length > AI_CHAT_PROVIDER_PATTERN_MAX_LENGTH) return 'too_long'
  if (!pattern.startsWith('https://')) return 'https_prefix'
  if (hasTopLevelAlternation(pattern)) return 'alternation'
  try {
    new RegExp(pattern, 'u')
    new RegExp(`^(?:${pattern})$`, 'u')
  } catch {
    return 'syntax'
  }
  return null
}

export type AiChatProviderKeyInvalidReason = 'format' | 'builtin' | 'duplicate'

export const validateAiChatProviderKey = (
  key: string,
  existing: readonly { key: string; builtin: boolean }[]
): AiChatProviderKeyInvalidReason | null => {
  if (!AI_CHAT_PROVIDER_KEY_PATTERN.test(key)) return 'format'
  const match = existing.find((provider) => provider.key === key)
  if (match) return match.builtin ? 'builtin' : 'duplicate'
  return null
}

/** Request path of the non-polled pending-disclosure read (refreshed after solves and saves). */
export const aiChatPendingPath = (gameId: number) => `/api/game/${gameId}/ai-chats/pending`

export interface AiChatDisclosureGate {
  /** The player state read succeeded and is current (no load error). */
  loaded: boolean
  required: boolean
  pending: boolean
  /** The team can still disclose; an expired window must never trap the dialog. */
  editable: boolean
}

/** The challenge dialog waits for a disclosure only when one is loaded, required, pending, and possible. */
export const aiChatDisclosureBlocksClose = ({ loaded, required, pending, editable }: AiChatDisclosureGate): boolean =>
  loaded && required && pending && editable

/** Pending challenges to announce on the event page, in catalog order; empty when nothing is required. */
export const aiChatPendingChallenges = <T extends { id: number }>(
  pending: { required: boolean; challengeIds: readonly number[] } | undefined,
  catalog: readonly T[]
): T[] => {
  if (!pending?.required || pending.challengeIds.length === 0) return []
  const ids = new Set(pending.challengeIds)
  return catalog.filter((challenge) => ids.has(challenge.id))
}

export type AiChatDelayUnit = 'seconds' | 'minutes' | 'hours' | 'days'

/** Whole-unit delay for "N min after solve" style labels; negative clock skew reads as zero. */
export const aiChatDelay = (seconds: number): { unit: AiChatDelayUnit; count: number } => {
  const value = Math.max(0, Math.floor(seconds))
  if (value < 60) return { unit: 'seconds', count: value }
  if (value < 3600) return { unit: 'minutes', count: Math.floor(value / 60) }
  if (value < 86400) return { unit: 'hours', count: Math.floor(value / 3600) }
  return { unit: 'days', count: Math.floor(value / 86400) }
}
