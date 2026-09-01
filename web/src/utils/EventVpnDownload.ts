import api from '@Api'

/** Download the caller's personal event WireGuard profile without navigating
 * away from the event or challenge modal. */
export const downloadEventVpnConfig = async (gameId: number) => {
  const response = await api.eventSecurity.gameVpnConfig(gameId)
  const blob = new Blob([response.data], { type: 'text/plain;charset=utf-8' })
  const url = URL.createObjectURL(blob)
  const anchor = document.createElement('a')

  try {
    anchor.href = url
    anchor.download = `rsctf-event-${gameId}.conf`
    document.body.appendChild(anchor)
    anchor.click()
  } finally {
    anchor.remove()
    URL.revokeObjectURL(url)
  }
}
