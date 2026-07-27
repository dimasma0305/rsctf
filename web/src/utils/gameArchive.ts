export const isReadOnlyGameArchive = (game?: { end?: number; practiceMode?: boolean }, now: number = Date.now()) =>
  typeof game?.end === 'number' && !game.practiceMode && now >= game.end
