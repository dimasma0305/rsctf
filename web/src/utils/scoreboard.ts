import { ChallengeInfo, ChallengeType } from '@Api'

/**
 * The jeopardy scoreboard must only show jeopardy challenges. A&D and KotH
 * challenges have their own independent boards (AdScoreboardTable /
 * KothScoreboardTable), but the shared scoreboard payload carries EVERY enabled
 * challenge — it also feeds the challenge list (`/Details`) and the backend
 * deliberately keeps A&D/KotH entries (with score 0) so the challenge cards still
 * render. So the jeopardy view has to strip the AD-engine challenges itself,
 * dropping any category that ends up empty.
 */
export const filterJeopardyChallenges = (
  challenges: Record<string, ChallengeInfo[]> | undefined
): Record<string, ChallengeInfo[]> | undefined => {
  if (!challenges) return challenges

  const result: Record<string, ChallengeInfo[]> = {}
  for (const [category, list] of Object.entries(challenges)) {
    const jeopardyOnly = list.filter(
      (c) => c.type !== ChallengeType.AttackDefense && c.type !== ChallengeType.KingOfTheHill
    )
    if (jeopardyOnly.length > 0) result[category] = jeopardyOnly
  }
  return result
}
