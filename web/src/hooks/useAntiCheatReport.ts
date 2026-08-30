import { useCallback, useEffect, useMemo, useRef } from 'react'
import { CHEAT_REPORT_REFRESH_INTERVAL_MS } from '@Utils/AntiCheat'
import { useChallengePolling } from '@Hooks/useChallengePolling'
import { jitterPollingDelay } from '@Hooks/useCompletionPolling'
import api, { type CheatReport } from '@Api'

type ResponseHeaders = { get?: (name: string) => unknown } | Record<string, unknown>

export type AntiCheatReportRead = {
  status: 200 | 304
  data?: CheatReport
  etag?: string
}

export type AntiCheatReportReader = (
  gameId: number,
  etag: string | undefined,
  signal: AbortSignal
) => Promise<AntiCheatReportRead>

const headerValue = (headers: ResponseHeaders | undefined, name: string): string | undefined => {
  if (!headers) return undefined
  const getter = (headers as { get?: unknown }).get
  const value =
    typeof getter === 'function'
      ? getter.call(headers, name)
      : ((headers as Record<string, unknown>)[name] ?? (headers as Record<string, unknown>)[name.toLowerCase()])
  return typeof value === 'string' ? value : undefined
}

const readAntiCheatReport: AntiCheatReportReader = async (gameId, etag, signal) => {
  const response = await api.request<CheatReport, unknown>({
    path: `/api/game/${gameId}/cheatreport`,
    method: 'GET',
    format: 'json',
    signal,
    headers: etag ? { 'If-None-Match': etag } : undefined,
    validateStatus: (status) => status === 304 || (status >= 200 && status < 300),
  })
  return {
    status: response.status as 200 | 304,
    data: response.status === 304 ? undefined : response.data,
    etag: headerValue(response.headers, 'etag'),
  }
}

/**
 * Own the analysis report request for the visible tab. Conditional reads retain
 * the last decoded report after a 304, and changing games/unmounting aborts the
 * in-flight generation through useChallengePolling.
 */
export const useAntiCheatReport = (
  gameId: number,
  active: boolean,
  reader: AntiCheatReportReader = readAntiCheatReport
) => {
  const etag = useRef<string | undefined>(undefined)
  const snapshot = useRef<CheatReport | undefined>(undefined)
  const cadence = useMemo(() => jitterPollingDelay(CHEAT_REPORT_REFRESH_INTERVAL_MS), [])

  useEffect(() => {
    etag.current = undefined
    snapshot.current = undefined
  }, [gameId])

  const request = useCallback(
    async (signal: AbortSignal) => {
      const response = await reader(gameId, etag.current, signal)

      if (response.status === 304) {
        if (!snapshot.current) throw new Error('Anti-cheat report returned 304 before an initial snapshot')
        return snapshot.current
      }

      if (!response.data) throw new Error('Anti-cheat report returned 200 without a snapshot')
      snapshot.current = response.data
      etag.current = response.etag
      return response.data
    },
    [gameId, reader]
  )

  return useChallengePolling<CheatReport>({
    key: active ? `/api/game/${gameId}/cheatreport#conditional` : null,
    active,
    refreshInterval: (report) => (report?.sealedAt == null ? cadence : 0),
    request,
  })
}
