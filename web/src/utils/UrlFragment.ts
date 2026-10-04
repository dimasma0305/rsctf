/** Preserve legacy anchors (including challenge title slugs) beside UI parameters. */
export const parseUrlFragment = (hash: string) => {
  const raw = hash.replace(/^#/, '')
  const separator = raw.indexOf('&')
  const first = separator === -1 ? raw : raw.slice(0, separator)
  const anchor = first && !first.includes('=') ? first : ''
  return {
    anchor,
    params: new URLSearchParams(anchor ? (separator === -1 ? '' : raw.slice(separator + 1)) : raw),
  }
}

export const updateUrlFragment = (hash: string, changes: Record<string, string | null>, anchor?: string) => {
  const current = parseUrlFragment(hash)
  for (const [key, value] of Object.entries(changes)) {
    if (value === null) current.params.delete(key)
    else current.params.set(key, value)
  }
  const value = [anchor ?? current.anchor, current.params.toString()].filter(Boolean).join('&')
  return value ? `#${value}` : ''
}
