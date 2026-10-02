import { useLocation, useNavigate } from 'react-router'

/** Shareable section navigation, independent of other tabs and existing hash links. */
export function useUrlTab<T extends string>(
  key: string,
  choices: readonly T[],
  fallback: T
): [T, (next: string | null) => void] {
  const location = useLocation()
  const navigate = useNavigate()
  const selected = new URLSearchParams(location.search).get(key)
  const value = choices.find((choice) => choice === selected) ?? fallback

  const setValue = (next: string | null) => {
    if (next === null || !choices.some((choice) => choice === next) || next === selected) return
    const params = new URLSearchParams(location.search)
    params.set(key, next)
    void navigate(
      { pathname: location.pathname, search: `?${params}`, hash: location.hash },
      { preventScrollReset: true, state: location.state }
    )
  }

  return [value, setValue]
}
