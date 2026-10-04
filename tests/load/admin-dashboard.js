export const DASHBOARD_OPERATIONS = Object.freeze([
  { id: 'dashboard', path: '/api/admin/dashboard', kind: 'dashboard' },
  { id: 'trend_day', path: '/api/admin/submissiontrend?range=Day', kind: 'trend', rows: 24 },
  { id: 'trend_week', path: '/api/admin/submissiontrend?range=Week', kind: 'trend', rows: 7 },
  { id: 'trend_month', path: '/api/admin/submissiontrend?range=Month', kind: 'trend', rows: 30 },
  { id: 'trend_year', path: '/api/admin/submissiontrend?range=Year', kind: 'trend', rows: 12 },
  { id: 'reviews', path: '/api/admin/reviews?count=10&skip=0', kind: 'page' },
  { id: 'writeups', path: '/api/admin/writeups?count=10&skip=0', kind: 'page' },
  { id: 'cheats', path: '/api/admin/cheat-reports?count=10&skip=0', kind: 'page' },
])

const object = (value) => value !== null && typeof value === 'object' && !Array.isArray(value)

export function validDashboardResponse(operation, body) {
  if (!operation || !DASHBOARD_OPERATIONS.includes(operation)) return false
  if (operation.kind === 'dashboard') {
    return (
      object(body) &&
      object(body.systemStats) &&
      Array.isArray(body.topGames) &&
      body.topGames.length <= 5 &&
      ['userCount', 'teamCount', 'activeContainerCount'].every(
        (key) => Number.isSafeInteger(body.systemStats[key]) && body.systemStats[key] >= 0
      )
    )
  }
  if (operation.kind === 'trend') {
    return (
      Array.isArray(body) &&
      body.length === operation.rows &&
      body.every((bucket) => Number.isFinite(bucket?.time) && Number.isSafeInteger(bucket?.count) && bucket.count >= 0)
    )
  }
  return Array.isArray(body) && body.length <= 10
}
