import { type ContainerInstanceFilterOptionModel } from '@Api'

export const ADMIN_INSTANCE_PAGE_SIZE = 25

export interface AdminInstanceViewState {
  page: number
  team: ContainerInstanceFilterOptionModel | null
  challenge: ContainerInstanceFilterOptionModel | null
}

export const INITIAL_ADMIN_INSTANCE_VIEW: AdminInstanceViewState = {
  page: 1,
  team: null,
  challenge: null,
}

export type AdminInstanceViewAction =
  | { type: 'setPage'; page: number }
  | { type: 'setTeam'; option: ContainerInstanceFilterOptionModel | null }
  | { type: 'setChallenge'; option: ContainerInstanceFilterOptionModel | null }
  | { type: 'reconcileTotal'; total: number }

export const reduceAdminInstanceView = (
  state: AdminInstanceViewState,
  action: AdminInstanceViewAction
): AdminInstanceViewState => {
  switch (action.type) {
    case 'setPage':
      return { ...state, page: Math.max(1, Math.trunc(action.page)) }
    case 'setTeam':
      return { ...state, page: 1, team: action.option }
    case 'setChallenge':
      return { ...state, page: 1, challenge: action.option }
    case 'reconcileTotal': {
      const lastPage = Math.max(1, Math.ceil(Math.max(0, action.total) / ADMIN_INSTANCE_PAGE_SIZE))
      return state.page > lastPage ? { ...state, page: lastPage } : state
    }
  }
}

export const adminInstancePageQuery = (state: AdminInstanceViewState, liveStats: boolean) => ({
  count: ADMIN_INSTANCE_PAGE_SIZE,
  skip: (state.page - 1) * ADMIN_INSTANCE_PAGE_SIZE,
  includeRuntimeStats: liveStats,
  teamId: state.team?.id,
  challengeId: state.challenge?.id,
})

/** Keep the active selection available while a newer remote search is loading. */
export const mergeAdminInstanceFilterOptions = (
  options: ContainerInstanceFilterOptionModel[],
  selected: ContainerInstanceFilterOptionModel | null
) => (selected && !options.some((option) => option.id === selected.id) ? [selected, ...options] : options)
