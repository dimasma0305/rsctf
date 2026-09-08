import type { WriteupGradeMode, WriteupGradeTeam } from '../Api'

export type GradingView = 'Overall' | WriteupGradeMode
const rounded = (value: number) => Math.round(Math.max(0, value) * 10000) / 10000

export function gradeScore(team: WriteupGradeTeam, view: GradingView, graded: boolean) {
  const cells = team.challenges.filter((c) => view === 'Overall' || c.mode === view)
  const original = view === 'Overall' ? team.originalScore : cells.reduce((sum, c) => sum + c.earnedPoints, 0)
  const deduction = graded
    ? cells.reduce(
        (sum, c) => sum + (view === 'Overall' ? c.overallPoints : c.earnedPoints) * (1 - (c.percentage ?? 100) / 100),
        0
      )
    : 0
  return rounded(original - deduction)
}

export function rankWriteupTeams(teams: WriteupGradeTeam[], view: GradingView, division: string) {
  const rows = teams
    .filter((t) => !division || String(t.divisionId) === division)
    .map((team) => ({ team, score: gradeScore(team, view, true), original: gradeScore(team, view, false), rank: 0 }))
    .sort((a, b) => b.score - a.score || a.team.teamId - b.team.teamId)
  let position = 0,
    previous = -1,
    rank = 0
  for (const row of rows) {
    if (!(division ? row.team.divisionEligible : row.team.overallEligible)) continue
    position++
    if (previous !== row.score) rank = position
    previous = row.score
    row.rank = rank
  }
  return rows
}
