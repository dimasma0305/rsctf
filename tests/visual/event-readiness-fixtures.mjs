import { fixture as navigationFixture } from './admin-navigation-fixtures.mjs'

export function fixture(path, method = 'GET', scenario = 'normal', role = 'Admin') {
  if (!['GET', 'HEAD'].includes(method)) return { status: 405, body: { title: 'Fixture write blocked' } }
  const pathname = new URL(path, 'http://localhost').pathname.toLowerCase()
  if (/^\/api\/edit\/games\/19(?:\/challenges)?$/.test(pathname)) {
    if (!['Admin', 'Manager'].includes(role)) return { status: 403, body: { title: 'forbidden' } }
    if (scenario === 'loading') return { hold: true }
    const status = { session: 401, denied: 403, missing: 404, busy: 429, failed: 503 }[scenario]
    if (status) return { status, body: { status, title: 'Fixture unavailable' } }
    const game = {
      id: 19, title: 'Readiness rehearsal · no live changes', sourceRevision: 1,
      start: 1_800_000_000_000, end: 1_800_003_600_000, serverTime: 1_799_996_400_000,
      freeze: 1_800_002_600_000, writeupRequired: true, writeupDeadline: 1_800_010_800_000,
      hidden: true, practiceMode: false, vpnAccessRequired: true,
    }
    const challenges = [
      { id: 74, title: 'Build needs review', isEnabled: true, reviewStatus: 'Active', type: 'AttackDefense', buildStatus: 'Failed' },
      { id: 75, title: 'Prebuilt hill', isEnabled: true, reviewStatus: 'Active', type: 'KingOfTheHill', buildStatus: 'None', hasOriginalArchive: true },
      { id: 76, title: 'Pending approval', isEnabled: true, reviewStatus: 'Pending', type: 'StaticAttachment', buildStatus: 'NotApplicable' },
    ]
    return { body: pathname.endsWith('/challenges') ? scenario === 'empty' ? [] : challenges : game }
  }
  return navigationFixture(path, method, role)
}
