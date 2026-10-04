import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { challengePollRetryDelay, isAbortError } from '@Utils/ChallengePolling'
import { LatestRequest } from '@Utils/LatestRequest'
import api, { TrafficFlowDetail, TrafficFlowPage, TrafficFlowQuery } from '@Api'

export const MAX_TRAFFIC_FLOW_RETRIES = 3

type Timer = ReturnType<typeof setTimeout>

/** One abortable request generation and at most one completion-scheduled retry. */
export class TrafficFlowRequestOwner {
  private readonly latest = new LatestRequest()
  private retryTimer: Timer | null = null

  run<T>(request: (signal: AbortSignal) => Promise<T>) {
    this.cancelRetry()
    return this.latest.run(request)
  }

  schedule(delay: number, action: () => void) {
    this.cancelRetry()
    this.retryTimer = setTimeout(() => {
      this.retryTimer = null
      action()
    }, delay)
  }

  cancelRetry() {
    if (this.retryTimer !== null) clearTimeout(this.retryTimer)
    this.retryTimer = null
  }

  cancel() {
    this.latest.cancel()
    this.cancelRetry()
  }

  pendingRetryCount() {
    return this.retryTimer === null ? 0 : 1
  }
}

type ErrorResponse = {
  response?: {
    data?: unknown
  }
}

export const trafficFlowErrorMessage = (error: unknown) => {
  if (error && typeof error === 'object') {
    const data = (error as ErrorResponse).response?.data
    if (data && typeof data === 'object') {
      const title = (data as { title?: unknown }).title
      if (typeof title === 'string' && title.trim()) return title
    }
  }
  return error instanceof Error && error.message ? error.message : String(error ?? 'Flow inspection failed')
}

export const trafficFlowRetryDelay = (
  error: unknown,
  completedFailures: number,
  random: () => number = Math.random,
  now?: number
) => {
  if (completedFailures > MAX_TRAFFIC_FLOW_RETRIES) return null
  return challengePollRetryDelay(error, completedFailures - 1, random, now)
}

export interface TrafficFlowLoadState {
  fileScope: string
  page: TrafficFlowPage | null
  loading: boolean
  error: string | null
  retryAfterMs: number | null
}

export const beginTrafficFlowLoad = (current: TrafficFlowLoadState, fileScope: string): TrafficFlowLoadState => ({
  fileScope,
  page: current.fileScope === fileScope ? current.page : null,
  loading: true,
  error: null,
  retryAfterMs: null,
})

export const failTrafficFlowLoad = (
  current: TrafficFlowLoadState,
  fileScope: string,
  error: unknown,
  retryAfterMs: number | null
): TrafficFlowLoadState => ({
  fileScope,
  page: current.fileScope === fileScope ? current.page : null,
  loading: false,
  error: trafficFlowErrorMessage(error),
  retryAfterMs,
})

interface UseTrafficFlowPageOptions {
  opened: boolean
  challengeId: number | null
  participationId: number | null
  filename: string | null
  query: TrafficFlowQuery
}

const EMPTY_STATE: TrafficFlowLoadState = {
  fileScope: '',
  page: null,
  loading: false,
  error: null,
  retryAfterMs: null,
}

export const useTrafficFlowPage = ({
  opened,
  challengeId,
  participationId,
  filename,
  query,
}: UseTrafficFlowPageOptions) => {
  const owner = useRef(new TrafficFlowRequestOwner())
  const generation = useRef(0)
  const [reloadVersion, setReloadVersion] = useState(0)
  const [state, setState] = useState<TrafficFlowLoadState>(EMPTY_STATE)
  const fileScope = JSON.stringify([challengeId, participationId, filename])
  const requestQuery = useMemo<TrafficFlowQuery>(
    () => ({ ...query }),
    [
      query.regexPattern,
      query.peerIpContains,
      query.direction,
      query.flagsOnly,
      query.startUtc,
      query.endUtc,
      query.page,
      query.pageSize,
    ]
  )

  useEffect(() => {
    const currentGeneration = ++generation.current
    owner.current.cancel()
    if (!opened || challengeId === null || participationId === null || filename === null) {
      setState(EMPTY_STATE)
      return
    }

    setState((current) => beginTrafficFlowLoad(current, fileScope))
    const load = async (completedFailures: number) => {
      try {
        const response = await owner.current.run((signal) =>
          api.game.gameGetTrafficFlows(challengeId, participationId, filename, requestQuery, { signal })
        )
        if (!response || generation.current !== currentGeneration) return
        setState({
          fileScope,
          page: response.data,
          loading: false,
          error: null,
          retryAfterMs: null,
        })
      } catch (error) {
        if (generation.current !== currentGeneration || isAbortError(error)) return
        const nextFailures = completedFailures + 1
        const retryAfterMs = trafficFlowRetryDelay(error, nextFailures)
        setState((current) => failTrafficFlowLoad(current, fileScope, error, retryAfterMs))
        if (retryAfterMs !== null) {
          owner.current.schedule(retryAfterMs, () => {
            if (generation.current !== currentGeneration) return
            setState((current) => beginTrafficFlowLoad(current, fileScope))
            void load(nextFailures)
          })
        }
      }
    }
    void load(0)
    return () => owner.current.cancel()
  }, [opened, challengeId, participationId, filename, fileScope, requestQuery, reloadVersion])

  const retry = useCallback(() => setReloadVersion((version) => version + 1), [])
  return { ...state, retry }
}

interface UseTrafficFlowDetailOptions {
  enabled: boolean
  challengeId: number
  participationId: number
  filename: string
  connectionPort: number | null
  flowId: string | null
  snapshotVersion: string | null
}

export interface TrafficFlowDetailLoadState {
  detail: TrafficFlowDetail | null
  loading: boolean
  error: string | null
  retryAfterMs: number | null
}

const EMPTY_DETAIL_STATE: TrafficFlowDetailLoadState = {
  detail: null,
  loading: false,
  error: null,
  retryAfterMs: null,
}

export const useTrafficFlowDetail = ({
  enabled,
  challengeId,
  participationId,
  filename,
  connectionPort,
  flowId,
  snapshotVersion,
}: UseTrafficFlowDetailOptions) => {
  const owner = useRef(new TrafficFlowRequestOwner())
  const generation = useRef(0)
  const [reloadVersion, setReloadVersion] = useState(0)
  const [state, setState] = useState<TrafficFlowDetailLoadState>(EMPTY_DETAIL_STATE)

  useEffect(() => {
    const currentGeneration = ++generation.current
    owner.current.cancel()
    if (!enabled || connectionPort === null || flowId === null || snapshotVersion === null) {
      setState(EMPTY_DETAIL_STATE)
      return
    }
    setState({ detail: null, loading: true, error: null, retryAfterMs: null })
    const load = async (completedFailures: number) => {
      try {
        const response = await owner.current.run((signal) =>
          api.game.gameGetTrafficFlowDetail(
            challengeId,
            participationId,
            filename,
            connectionPort,
            { snapshotVersion, flowId },
            { signal }
          )
        )
        if (!response || generation.current !== currentGeneration) return
        setState({ detail: response.data, loading: false, error: null, retryAfterMs: null })
      } catch (error) {
        if (generation.current !== currentGeneration || isAbortError(error)) return
        const nextFailures = completedFailures + 1
        const retryAfterMs = trafficFlowRetryDelay(error, nextFailures)
        setState((current) => ({
          detail: current.detail,
          loading: false,
          error: trafficFlowErrorMessage(error),
          retryAfterMs,
        }))
        if (retryAfterMs !== null) {
          owner.current.schedule(retryAfterMs, () => {
            if (generation.current !== currentGeneration) return
            setState((current) => ({ ...current, loading: true, error: null, retryAfterMs: null }))
            void load(nextFailures)
          })
        }
      }
    }
    void load(0)
    return () => owner.current.cancel()
  }, [enabled, challengeId, participationId, filename, connectionPort, flowId, snapshotVersion, reloadVersion])

  const retry = useCallback(() => setReloadVersion((version) => version + 1), [])
  return { ...state, retry }
}
