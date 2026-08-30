import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { LatestRequest } from '@Utils/LatestRequest'
import { useChallengePolling } from '@Hooks/useChallengePolling'
import { jitterPollingDelay } from '@Hooks/useCompletionPolling'
import api, { type CheatIncidentPage, type CheatIncidentPageItem } from '@Api'

const INCIDENT_PAGE_SIZE = 100
const INCIDENT_DELTA_INTERVAL_MS = 10_000
const MAX_IMMEDIATE_DELTA_PAGES = 8

type FeedRead = {
  kind: 'initial' | 'delta'
  page: CheatIncidentPage
}

export type CheatIncidentPageQuery = {
  limit: number
  afterId?: number
  beforeObservedAt?: number
  beforeId?: number
}

export type CheatIncidentPageReader = (
  gameId: number,
  query: CheatIncidentPageQuery,
  signal: AbortSignal
) => Promise<CheatIncidentPage>

const readCheatIncidentPage: CheatIncidentPageReader = async (gameId, query, signal) => {
  const response = await api.request<CheatIncidentPage, unknown>({
    path: `/api/game/${gameId}/cheatinfo/page`,
    method: 'GET',
    query,
    format: 'json',
    signal,
  })
  return response.data
}

const mergeIncidentDelta = (current: CheatIncidentPageItem[], delta: CheatIncidentPageItem[]) => {
  const rows = new Map(current.map((row) => [row.id, row]))
  for (const row of delta) rows.set(row.id, row)
  return [...rows.values()].sort((left, right) => right.observedAt - left.observedAt || right.id - left.id)
}

/**
 * Visible-tab owner for the bounded incident ledger. The live request advances
 * from one stable ID, while older pages use the observedAt/id keyset and a
 * separate abortable request so neither path can replace the other.
 */
export const useCheatIncidentFeed = (
  gameId: number,
  active: boolean,
  reader: CheatIncidentPageReader = readCheatIncidentPage
) => {
  const [rows, setRows] = useState<CheatIncidentPageItem[]>([])
  const [nextBefore, setNextBefore] = useState<CheatIncidentPage['nextBefore']>(null)
  const [hasOlder, setHasOlder] = useState(false)
  const [olderError, setOlderError] = useState<unknown>()
  const [loadingOlder, setLoadingOlder] = useState(false)
  const checkpoint = useRef<number | null>(null)
  const immediateDeltaPages = useRef(0)
  const olderRequest = useMemo(() => new LatestRequest(), [])
  const cadence = useMemo(() => jitterPollingDelay(INCIDENT_DELTA_INTERVAL_MS), [])

  useEffect(() => {
    checkpoint.current = null
    immediateDeltaPages.current = 0
    olderRequest.cancel()
    setRows([])
    setNextBefore(null)
    setHasOlder(false)
    setOlderError(undefined)
    setLoadingOlder(false)
    return () => olderRequest.cancel()
  }, [active, gameId, olderRequest])

  const request = useCallback(
    async (signal: AbortSignal): Promise<FeedRead> => {
      const afterId = checkpoint.current
      const page = await reader(
        gameId,
        afterId === null ? { limit: INCIDENT_PAGE_SIZE } : { limit: INCIDENT_PAGE_SIZE, afterId },
        signal
      )
      return { kind: afterId === null ? 'initial' : 'delta', page }
    },
    [gameId, reader]
  )

  const poll = useChallengePolling<FeedRead>({
    key: active ? `/api/game/${gameId}/cheatinfo/page#feed` : null,
    active,
    refreshInterval: cadence,
    request,
  })

  useEffect(() => {
    if (!poll.data) return
    const { kind, page } = poll.data
    if (kind === 'initial') {
      setRows(page.data)
      setNextBefore(page.nextBefore)
      setHasOlder(page.hasMore)
      checkpoint.current = page.checkpointId
      immediateDeltaPages.current = 0
      return
    }

    setRows((current) => mergeIncidentDelta(current, page.data))
    checkpoint.current = page.checkpointId
    if (page.hasMore && immediateDeltaPages.current < MAX_IMMEDIATE_DELTA_PAGES) {
      immediateDeltaPages.current += 1
      void poll.mutate()
    } else if (!page.hasMore) {
      immediateDeltaPages.current = 0
    }
  }, [poll.data, poll.mutate])

  const loadOlder = useCallback(async () => {
    const cursor = nextBefore
    if (!active || !cursor) return
    setLoadingOlder(true)
    setOlderError(undefined)
    try {
      const page = await olderRequest.run(async (signal) => {
        return reader(
          gameId,
          {
            limit: INCIDENT_PAGE_SIZE,
            beforeObservedAt: cursor.observedAt,
            beforeId: cursor.id,
          },
          signal
        )
      })
      if (!page) return
      setRows((current) => mergeIncidentDelta(current, page.data))
      setNextBefore(page.nextBefore)
      setHasOlder(page.hasMore)
    } catch (error) {
      setOlderError(error)
    } finally {
      setLoadingOlder(false)
    }
  }, [active, gameId, nextBefore, olderRequest, reader])

  return {
    data: rows,
    error: poll.error ?? olderError,
    isLoading: poll.isLoading && rows.length === 0,
    isValidating: poll.isValidating,
    loadingOlder,
    hasOlder,
    mutate: poll.mutate,
    loadOlder,
    updateRows: setRows,
  }
}
