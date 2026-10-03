export type ArenaRanking = 'ad' | 'koth' | 'jeopardy'

/** Schematic front-hemisphere positions, not team geolocation. Leave room for hills. */
export function arenaTeamPosition(index: number, count: number) {
  const angle = index * Math.PI * (3 - Math.sqrt(5)) - Math.PI / 2
  const radius = Math.sqrt(220 ** 2 + ((index + 0.5) / Math.max(1, count)) * (365 ** 2 - 220 ** 2))
  return { x: 500 + radius * Math.cos(angle), y: 500 + radius * Math.sin(angle), angle }
}

export function initialArenaRanking(services: number, hills: number): ArenaRanking {
  return services > 0 ? 'ad' : hills > 0 ? 'koth' : 'jeopardy'
}

export function arenaTeamInitials(name: string) {
  return name
    .trim()
    .split(/\s+/)
    .slice(0, 2)
    .map((word) => Array.from(word)[0] || '')
    .join('')
    .toLocaleUpperCase()
}

export function arenaTeamLabel(name: string) {
  const letters = Array.from(name)
  return letters.length > 16 ? letters.slice(0, 15).join('') + '…' : name
}
