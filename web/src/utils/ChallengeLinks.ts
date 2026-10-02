/** Keep existing event challenge bookmarks compatible, including title slugs. */
export const eventChallengeHash = (id: number, title: string) =>
  `#${id}-${encodeURIComponent(title.replace(/ /g, '-'))}`

export const catalogChallengeIdFromHash = (hash: string): number | null => {
  const values = new URLSearchParams(hash.replace(/^#/, '')).getAll('challenge')
  if (values.length !== 1 || !/^[1-9]\d{0,9}$/.test(values[0])) return null
  const id = Number(values[0])
  return id <= 2_147_483_647 ? id : null
}

export const catalogChallengeHash = (hash: string, id: number | null) => {
  const params = new URLSearchParams(hash.replace(/^#/, ''))
  if (id === null) params.delete('challenge')
  else params.set('challenge', String(id))
  const value = params.toString()
  return value ? `#${value}` : ''
}
