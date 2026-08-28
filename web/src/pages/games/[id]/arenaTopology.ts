interface ArenaScoreboardLike {
  challenges?: Array<{ challengeId?: unknown; title?: unknown }>
  teams?: unknown[]
}

interface KothScoreboardLike {
  hills?: Array<{ challengeId?: unknown; title?: unknown }>
  teams?: unknown[]
}

interface JeopardyScoreboardLike {
  items?: unknown[]
}

const integer = (value: unknown): number | null => (Number.isInteger(value) ? (value as number) : null)

const rosterIdentity = (row: any) => ({
  teamId: integer(row?.teamId) ?? integer(row?.id),
  participationId: integer(row?.participationId),
  teamName: String(row?.teamName ?? row?.name ?? ''),
})

/** Build the same stable, de-duplicated cross-format roster used by the live arena. */
export const buildArenaRosterRows = (
  ad: ArenaScoreboardLike | null | undefined,
  koth: KothScoreboardLike | null | undefined,
  jeopardy: JeopardyScoreboardLike | null | undefined
): any[] => {
  const rows: any[] = []
  const seenTeamIds = new Set<number>()
  const seenNames = new Set<string>()
  const add = (row: any) => {
    const identity = rosterIdentity(row)
    if (
      !identity.teamName ||
      (identity.teamId !== null ? seenTeamIds.has(identity.teamId) : seenNames.has(identity.teamName))
    ) {
      return
    }
    if (identity.teamId !== null) seenTeamIds.add(identity.teamId)
    seenNames.add(identity.teamName)
    rows.push({ ...row, teamId: identity.teamId, teamName: identity.teamName })
  }

  ;(ad?.teams ?? []).forEach(add)
  ;(koth?.teams ?? []).forEach(add)
  ;(jeopardy?.items ?? []).forEach(add)
  return rows
}

/** Visible topology only; score changes intentionally do not rebuild the arena DOM. */
export const arenaTopologySignature = (
  ad: ArenaScoreboardLike | null | undefined,
  koth: KothScoreboardLike | null | undefined,
  jeopardy: JeopardyScoreboardLike | null | undefined
): string => {
  const hills = koth?.hills ?? []
  const hillIds = new Set(hills.map((hill) => integer(hill.challengeId)).filter((id): id is number => id !== null))
  const services = (ad?.challenges ?? []).filter((challenge) => {
    const id = integer(challenge.challengeId)
    return id !== null && !hillIds.has(id)
  })
  const roster = buildArenaRosterRows(ad, koth, jeopardy).map(rosterIdentity)

  return JSON.stringify({
    roster,
    services: services.map((service) => [integer(service.challengeId), String(service.title ?? '')]),
    hills: hills.map((hill) => [integer(hill.challengeId), String(hill.title ?? '')]),
  })
}
