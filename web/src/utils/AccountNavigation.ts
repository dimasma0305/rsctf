/** Keep account detours on this origin and preserve their query/hash destination. */
export const accountReturnPath = (value: string | null | undefined): string =>
  value?.startsWith('/') && !value.startsWith('//') && !value.startsWith('/\\') && !/[\p{Cc} ]/u.test(value)
    ? value
    : '/'

export const accountPath = (path: string, from: string | null | undefined): string => {
  const destination = accountReturnPath(from)
  return destination === '/' ? path : `${path}?from=${encodeURIComponent(destination)}`
}
