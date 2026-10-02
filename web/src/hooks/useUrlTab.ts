import { useLocation, useNavigate } from 'react-router'
import { parseUrlFragment, updateUrlFragment } from '@Utils/UrlFragment'

/** Hash-backed UI navigation, with read compatibility for older query bookmarks. */
export function useUrlTab<T extends string>(
  key: string,
  choices: readonly T[],
  fallback: T
): [T, (next: string | null) => void] {
  const location = useLocation()
  const navigate = useNavigate()
  const hashSelected = parseUrlFragment(location.hash).params.get(key)
  const selected = hashSelected ?? new URLSearchParams(location.search).get(key)
  const value = choices.find((choice) => choice === selected) ?? fallback

  const setValue = (next: string | null) => {
    if (next === null || !choices.some((choice) => choice === next) || next === hashSelected) return
    const params = new URLSearchParams(location.search)
    params.delete(key)
    const query = params.toString()
    void navigate(
      {
        pathname: location.pathname,
        search: query ? `?${query}` : '',
        hash: updateUrlFragment(location.hash, { [key]: next }),
      },
      { preventScrollReset: true, state: location.state }
    )
  }

  return [value, setValue]
}
