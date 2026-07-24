export interface ContainerOwnerLabels {
  shared: string
  adminTest: string
  exercise: string
  unassigned: string
}

interface ContainerInstanceOwnership {
  team?: { name?: string } | null
  ownerKind?: 'Team' | 'Shared' | 'AdminTest' | 'Exercise' | 'Unassigned'
  ownerName?: string | null
}

/** Render a real team or an explicit non-team ownership scope for admin rows. */
export const containerOwnerLabel = (instance: ContainerInstanceOwnership, labels: ContainerOwnerLabels): string => {
  if (instance.team?.name) return instance.team.name

  switch (instance.ownerKind) {
    case 'Shared':
      return labels.shared
    case 'AdminTest':
      return labels.adminTest
    case 'Exercise':
      return instance.ownerName ? `${labels.exercise}: ${instance.ownerName}` : labels.exercise
    default:
      return labels.unassigned
  }
}

/** Only proxy-enabled rows have a usable `/api/proxy/{uuid}` WebSocket URL. */
export const hasContainerProxy = (instance: {
  containerGuid?: string
  isProxy?: boolean
}): instance is {
  containerGuid: string
  isProxy: true
} => instance.isProxy === true && !!instance.containerGuid
