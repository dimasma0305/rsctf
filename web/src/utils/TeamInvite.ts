const TEAM_INVITE_CODE_PATTERN = /:\d+:[0-9a-f]{32}$/

export const isValidTeamInviteCode = (value: string) => TEAM_INVITE_CODE_PATTERN.test(value)

/** Accept a copied invitation link without forwarding invitations to another site. */
export const teamInviteFromInput = (input: string, origin: string): string => {
  const value = input.trim()
  try {
    const url = new URL(value, origin)
    if (url.origin === origin && url.pathname.toLowerCase() === '/teams') {
      const code = url.searchParams.get('join') ?? ''
      if (isValidTeamInviteCode(code)) return code
    }
  } catch {
    /* A plain code is also accepted. */
  }
  // Normalize a complete pasted code, but keep spaces while a name is being typed.
  return isValidTeamInviteCode(value) ? value : input
}

export const isTeamInviteDestination = (path: string): boolean => {
  if (!path.startsWith('/teams?')) return false
  return isValidTeamInviteCode(new URLSearchParams(path.slice(path.indexOf('?') + 1)).get('join') ?? '')
}
