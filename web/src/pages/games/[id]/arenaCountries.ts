import type { AdScoreboardModel, ScoreboardModel } from '@Api'
import { acceptedTerritorySolvers, type PublicSolver } from './arenaGlobeModel'
import type { JeopCategory, JeopChallenge } from './arenaJeopardy'

export interface ArenaCountry extends JeopChallenge {
  mode?: 'jeopardy' | 'ad' | 'koth'
  holder?: PublicSolver | null
  holderKnown?: boolean
}
export interface ArenaCategory extends JeopCategory {
  challenges: ArenaCountry[]
}
interface PublicHill {
  challengeId: number
  title: string
  category?: string
  currentHolderTeamName?: string | null
}
export const CATEGORY_COLOR: Record<string, string> = {
  Misc: '#46e3a0',
  Crypto: '#ffc637',
  Pwn: '#ff4d6a',
  Web: '#34e3ff',
  Reverse: '#a06bff',
  Blockchain: '#ff8c42',
  Forensics: '#ff5bd0',
  Hardware: '#8bd450',
  Mobile: '#5b8cff',
  PPC: '#ff6f91',
  AI: '#2ee6c0',
  Pentest: '#e0b24a',
  OSINT: '#b07bff',
}

/** Use only the existing public snapshots; a service or hill is never a Jeopardy solve. */
export function buildArenaCountries(
  ad: Pick<AdScoreboardModel, 'challenges'>,
  koth: { hills?: PublicHill[] } | null,
  board: Pick<ScoreboardModel, 'challenges' | 'items'> | null,
  colorOf: (name: string) => string
): ArenaCategory[] {
  const services = new Map((ad?.challenges || []).map((c) => [c.challengeId, c]))
  const hills = new Map((koth?.hills || []).map((h) => [h.challengeId, h]))
  const groups = new Map<string, ArenaCategory>()
  const seen = new Set<number>()
  const add = (category: string, country: ArenaCountry) => {
    if (seen.has(country.id)) return
    seen.add(country.id)
    if (!groups.has(category))
      groups.set(category, {
        id: category,
        name: category.toUpperCase(),
        color: CATEGORY_COLOR[category] || '#7fd7ff',
        challenges: [],
      })
    groups.get(category)!.challenges.push(country)
  }
  const live = (id: number, name: string, mode: 'ad' | 'koth'): ArenaCountry => {
    const hill = hills.get(id)
    const holder = hill?.currentHolderTeamName
    return {
      id,
      name,
      mode,
      base: 0,
      solveCount: 0,
      solvers: [],
      holderKnown: !!hill,
      holder: holder ? { name: holder, color: colorOf(holder) } : null,
    }
  }
  for (const [category, challenges] of Object.entries(board?.challenges || {})) {
    for (const c of challenges) {
      if (hills.has(c.id) || c.type === 'KingOfTheHill') add(category, live(c.id, c.title, 'koth'))
      else if (services.has(c.id) || c.type === 'AttackDefense') add(category, live(c.id, c.title, 'ad'))
      else
        add(category, {
          id: c.id,
          name: c.title,
          mode: 'jeopardy',
          base: Math.round(c.score || 0),
          solveCount: c.solved || 0,
          solvers: acceptedTerritorySolvers(c.id, board?.items || [], c.bloods || [], colorOf),
        })
    }
  }
  // Mode scoreboards can arrive before the general catalog. Keep their public entries visible.
  for (const c of services.values()) add(c.category || 'A&D', live(c.challengeId, c.title, 'ad'))
  for (const h of hills.values()) add(h.category || 'KotH', live(h.challengeId, h.title, 'koth'))
  return [...groups.values()]
}

export const isJeopardyCountry = (country: ArenaCountry) => !country.mode || country.mode === 'jeopardy'
export const countryColor = (country: ArenaCountry) =>
  country.mode === 'koth' ? country.holder?.color : country.solvers[0]?.color
export function countryStatus(country: ArenaCountry) {
  if (country.mode === 'ad') return 'A&D · Per-team services'
  if (country.mode === 'koth')
    return `KotH · ${country.holder ? `Held by ${country.holder.name}` : country.holderKnown ? 'No current holder' : 'Holder unavailable'}`
  return `${country.base} pts · ${country.solveCount ? `${country.solveCount} solves` : 'Unconquered'}`
}
