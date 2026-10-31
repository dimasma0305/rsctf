import useSWR from 'swr'
import { useViewerIdentity, viewerScopedKey } from '@Utils/ViewerIdentity'
import api from '@Api'

export interface CompetitionMaintenanceWindow {
  id: string
  gameId: number
  state: 'Requested' | 'Editing' | 'Applying' | 'Completed' | 'Cancelled'
  effectiveFromRound: number | null
  error: string | null
}

export const useCompetitionMaintenance = (gameId: number, enabled = true) => {
  const { scope } = useViewerIdentity()
  const path = `/api/edit/games/${gameId}/ad/maintenance`
  return useSWR<CompetitionMaintenanceWindow | null>(
    viewerScopedKey(enabled && gameId > 0 ? path : null, scope),
    async () => (await api.request<CompetitionMaintenanceWindow | null>({ path, method: 'GET' })).data,
    {
      refreshInterval: (window) => (window && ['Requested', 'Editing', 'Applying'].includes(window.state) ? 5000 : 0),
      refreshWhenHidden: false,
      refreshWhenOffline: false,
      keepPreviousData: false,
    }
  )
}
