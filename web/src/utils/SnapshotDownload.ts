export const progressLabel = (loaded: number, total: number) =>
  total > 0 ? `${Math.min(100, Math.round((loaded / total) * 100))}%` : '…'
