import { useDebouncedValue } from '@mantine/hooks'
import { useEffect, useRef, useState } from 'react'
import useSWR from 'swr'
import { scoreboardPagePath, type ScoreboardPaging } from '@Utils/ScoreboardPage'
import { useViewerIdentity } from '@Utils/ViewerIdentity'
import { CompletionPollSWRConfig } from '@Hooks/useCompletionPolling'
import type { ScoreboardPageModel, RequestResponse } from '@Api'

/** One request/polling owner for the visible page, including mobile and search. */
export const useScoreboardPage = (gameId: number, divisionId: number | null, mobile: boolean) => {
  const { scope } = useViewerIdentity()
  const identity = `${gameId}:${scope ?? 'unscoped'}`
  const retained = useRef<{ identity: string; board: ScoreboardPageModel } | undefined>(undefined)
  const [requestedPage, setRequestedPage] = useState(1)
  const [keyword, setKeyword] = useState('')
  const [search] = useDebouncedValue(keyword, 300)
  const [locateTeam, setLocateTeam] = useState<string | undefined>()
  const count = mobile ? 10 : 30
  const key = scoreboardPagePath(gameId, {
    skip: (requestedPage - 1) * count, count, divisionId, search, locateTeam,
  })
  const { data, error, isValidating, mutate } = useSWR<ScoreboardPageModel, RequestResponse>(
    gameId > 0 ? key : null,
    { ...CompletionPollSWRConfig, compare: Object.is }
  )
  if (retained.current?.identity !== identity) retained.current = undefined
  if (data) retained.current = { identity, board: data }
  const scoreboard = data ?? retained.current?.board

  useEffect(() => {
    setRequestedPage(1)
    setLocateTeam(undefined)
  }, [gameId, divisionId, mobile])
  useEffect(() => setKeyword(''), [gameId])

  const paging: ScoreboardPaging = {
    page: scoreboard ? Math.floor(scoreboard.skip / count) + 1 : requestedPage,
    skip: scoreboard?.skip ?? (requestedPage - 1) * count,
    count,
    total: scoreboard?.total ?? 0,
    pending: data === undefined && !error,
    keyword,
    setKeyword: (value) => { setKeyword(value); setRequestedPage(1); setLocateTeam(undefined) },
    setPage: (value) => { setRequestedPage(Math.max(1, value)); setLocateTeam(undefined) },
    findTeam: (name) => setLocateTeam(name),
  }
  return { scoreboard, error, isValidating, mutate, key, paging }
}
