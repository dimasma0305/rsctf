export interface AdTokenViewerScope {
  participationId: number
  teamId: number
}

export const adTokenViewerScope = (scope: AdTokenViewerScope | null | undefined) =>
  scope ? `${scope.participationId}:${scope.teamId}` : null

export const isCurrentAdTokenViewer = (
  expected: string | null,
  result: AdTokenViewerScope,
  current: string | null
) => expected !== null && expected === adTokenViewerScope(result) && expected === current
