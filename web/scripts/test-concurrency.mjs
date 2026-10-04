// Share the existing bounded frontend worker setting. CI can use its four-core
// runner, while local checks retain two workers unless explicitly configured.
export function testConcurrency(value = process.env.RSCTF_FRONTEND_WORKERS) {
  if (value === undefined) return 2
  if (typeof value !== 'string' || !/^[1-4]$/.test(value)) {
    throw new Error('RSCTF_FRONTEND_WORKERS must be an integer from 1 through 4')
  }
  return Number(value)
}
