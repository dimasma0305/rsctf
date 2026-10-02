import { parseUrlFragment, updateUrlFragment } from './UrlFragment'

/** Keep existing event challenge bookmarks compatible, including title slugs. */
export const eventChallengeHash = (id: number, title: string, hash = '') =>
  updateUrlFragment(hash, {}, `${id}-${encodeURIComponent(title.replace(/ /g, '-'))}`)

export const closeEventChallengeHash = (hash: string) => updateUrlFragment(hash, {}, '')

export const catalogChallengeIdFromHash = (hash: string): number | null => {
  const values = parseUrlFragment(hash).params.getAll('challenge')
  if (values.length !== 1 || !/^[1-9]\d{0,9}$/.test(values[0])) return null
  const id = Number(values[0])
  return id <= 2_147_483_647 ? id : null
}

export const catalogChallengeHash = (hash: string, id: number | null) =>
  updateUrlFragment(hash, { challenge: id === null ? null : String(id) })
