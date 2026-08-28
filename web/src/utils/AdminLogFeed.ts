import type { LogMessageModel } from '@Api'

export const ADMIN_LOG_PAGE_SIZE = 50
export const MAX_BUFFERED_ADMIN_LOGS = ADMIN_LOG_PAGE_SIZE
export const MAX_VISIBLE_ADMIN_LOGS = ADMIN_LOG_PAGE_SIZE * 2

export interface AdminLogQueryState {
  level: string
  page: number
  search: string
}

export type AdminLogQueryAction =
  { type: 'level'; level: string } | { type: 'page'; page: number } | { type: 'search'; search: string }

const normalizedPage = (page: number) => (Number.isFinite(page) ? Math.max(1, Math.floor(page)) : 1)

/** A filter commit and its page-one reset are one state transition, so React
 * cannot render or fetch a new filter with the previous page offset. */
export const adminLogQueryReducer = (state: AdminLogQueryState, action: AdminLogQueryAction): AdminLogQueryState => {
  switch (action.type) {
    case 'level':
      if (state.level === action.level && state.page === 1) return state
      return { ...state, level: action.level, page: 1 }
    case 'search':
      if (state.search === action.search && state.page === 1) return state
      return { ...state, search: action.search, page: 1 }
    case 'page': {
      const page = normalizedPage(action.page)
      return state.page === page ? state : { ...state, page }
    }
  }
}

export const adminLogQueryScope = ({ level, page, search }: AdminLogQueryState) => JSON.stringify([level, page, search])

export const adminLogIdentity = (item: LogMessageModel) => item.id

export const compareAdminLogsNewestFirst = (left: LogMessageModel, right: LogMessageModel) =>
  (right.time ?? 0) - (left.time ?? 0) || right.id - left.id

export const adminLogMatchesQuery = (item: LogMessageModel, query: AdminLogQueryState) => {
  if (query.level !== 'All' && item.level !== query.level) return false

  const search = query.search.trim().toLowerCase()
  if (!search) return true
  return [item.name, item.msg, item.ip, item.fingerprint].some(
    (value) => typeof value === 'string' && value.toLowerCase().includes(search)
  )
}
