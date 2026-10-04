export const isReadOnlyGameArchive = (game: { end?: number; practiceMode?: boolean } | undefined, now: number) =>
  typeof game?.end === 'number' && !game.practiceMode && now >= game.end
