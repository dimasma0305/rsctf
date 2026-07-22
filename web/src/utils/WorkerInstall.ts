export const workerInstallCommand = (origin: string): string => {
  const parsed = new URL(origin)
  if (parsed.protocol !== 'https:' || parsed.username || parsed.password || parsed.origin !== origin) {
    throw new Error('worker installation requires one exact HTTPS origin')
  }
  if (!/^[A-Za-z0-9.-]+(?::[0-9]{1,5})?$/.test(parsed.host)) {
    throw new Error('worker installation origin is not shell-safe')
  }
  return `curl -fsSL ${parsed.origin}/install/worker | sudo bash -s -- --server-url ${parsed.origin}`
}
