const workerInstallOrigin = (origin: string): string => {
  const parsed = new URL(origin)
  if (parsed.protocol !== 'https:' || parsed.username || parsed.password || parsed.origin !== origin) {
    throw new Error('worker installation requires one exact HTTPS origin')
  }
  if (!/^[A-Za-z0-9.-]+(?::[0-9]{1,5})?$/.test(parsed.host)) {
    throw new Error('worker installation origin is not shell-safe')
  }
  return parsed.origin
}

export const workerInstallCommand = (origin: string): string => {
  const safeOrigin = workerInstallOrigin(origin)
  return `curl -fsSL ${safeOrigin}/install/worker | sudo bash -s -- --server-url ${safeOrigin}`
}

export const workerWindowsInstallCommand = (origin: string): string => {
  const safeOrigin = workerInstallOrigin(origin)
  return `& ([scriptblock]::Create((Invoke-RestMethod ${safeOrigin}/install/worker.ps1))) -ServerUrl ${safeOrigin}`
}
