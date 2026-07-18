export interface PaginationState {
  page: number
  totalPages: number
}

/** Normalizes API- or filter-derived pagination before it reaches UI controls. */
export function getPaginationState(value: number, total: number): PaginationState {
  const totalPages = Number.isFinite(total) ? Math.max(1, Math.floor(total)) : 1
  const requestedPage = Number.isFinite(value) ? Math.floor(value) : 1

  return {
    page: Math.min(Math.max(requestedPage, 1), totalPages),
    totalPages,
  }
}
