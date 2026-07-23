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

const workerUnixCommand = (origin: string, arguments_: string): string => {
  const safeOrigin = workerInstallOrigin(origin)
  return `(t=$(mktemp) || exit 1; trap 'rm -f "$t"' 0 HUP INT TERM; wget -q -T 30 -O "$t" ${safeOrigin}/install/worker && sh "$t" ${arguments_})`
}

export const workerInstallCommand = (origin: string): string => {
  const safeOrigin = workerInstallOrigin(origin)
  return workerUnixCommand(safeOrigin, `--server-url ${safeOrigin}`)
}

export const workerWindowsInstallCommand = (origin: string): string => {
  const safeOrigin = workerInstallOrigin(origin)
  return `& ([scriptblock]::Create((Invoke-RestMethod ${safeOrigin}/install/worker.ps1))) -ServerUrl ${safeOrigin}`
}

export const workerUninstallCommand = (origin: string): string => {
  return workerUnixCommand(origin, '--uninstall')
}

export const workerWindowsUninstallCommand = (origin: string): string => {
  const safeOrigin = workerInstallOrigin(origin)
  return `& ([scriptblock]::Create((Invoke-RestMethod ${safeOrigin}/install/worker.ps1))) -Uninstall`
}
