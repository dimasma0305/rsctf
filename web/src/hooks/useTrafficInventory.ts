import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { LatestRequest } from '@Utils/LatestRequest'
import api, { type TrafficInventoryPage } from '@Api'

const TRAFFIC_PAGE_SIZE = 100

export type TrafficInventoryReader<T> = (
  path: string,
  cursor: string | null,
  signal: AbortSignal
) => Promise<TrafficInventoryPage<T>>

const readTrafficInventory = async <T>(path: string, cursor: string | null, signal: AbortSignal) => {
  const response = await api.request<TrafficInventoryPage<T>, unknown>({
    path,
    method: 'GET',
    query: { count: TRAFFIC_PAGE_SIZE, ...(cursor ? { cursor } : {}) },
    format: 'json',
    signal,
  })
  return response.data
}

/** One abortable page owner for a traffic navigation scope. */
export const useTrafficInventory = <T>(
  path: string | null,
  keyOf: (item: T) => string,
  reader: TrafficInventoryReader<T> = readTrafficInventory
) => {
  const [items, setItems] = useState<T[]>([])
  const [nextCursor, setNextCursor] = useState<string | null>(null)
  const [error, setError] = useState<unknown>()
  const [isLoading, setIsLoading] = useState(false)
  const request = useMemo(() => new LatestRequest(), [])
  const sequence = useRef(0)

  const read = useCallback(
    async (cursor: string | null, append: boolean) => {
      if (!path) return
      const currentSequence = ++sequence.current
      setIsLoading(true)
      setError(undefined)
      try {
        const page = await request.run(async (signal) => {
          return reader(path, cursor, signal)
        })
        if (!page) return
        setItems((current) => {
          if (!append) return page.items
          const merged = new Map(current.map((item) => [keyOf(item), item]))
          for (const item of page.items) merged.set(keyOf(item), item)
          return [...merged.values()]
        })
        setNextCursor(page.nextCursor)
      } catch (readError) {
        setError(readError)
      } finally {
        if (sequence.current === currentSequence) setIsLoading(false)
      }
    },
    [keyOf, path, reader, request]
  )

  useEffect(() => {
    sequence.current += 1
    request.cancel()
    setItems([])
    setNextCursor(null)
    setError(undefined)
    setIsLoading(false)
    if (path) void read(null, false)
    return () => {
      sequence.current += 1
      request.cancel()
    }
  }, [path, read, request])

  return {
    items,
    error,
    isLoading,
    hasMore: nextCursor !== null,
    loadMore: () => read(nextCursor, true),
    reload: () => read(null, false),
  }
}
