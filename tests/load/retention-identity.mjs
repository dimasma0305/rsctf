/** Match a retained manifest to the exact game identity created by its run. */
export function retainedManifestMatchesGame(manifest, gameId, currentTitle, filename = 'manifest') {
  if (manifest?.retained !== true) return false;
  const createdAtMs = Number(manifest.createdAtMs);
  if (!Number.isSafeInteger(createdAtMs) || createdAtMs <= 0) {
    throw new Error(`cannot verify retained lifecycle manifest ${filename}: invalid createdAtMs`);
  }
  const id = Number(gameId);
  if (!Number.isSafeInteger(id) || id <= 0) return false;
  if (id === Number(manifest.jeoGame)) return currentTitle === `LOADTEST-JEO-${createdAtMs}`;
  if (id === Number(manifest.mixGame)) return currentTitle === `LOADTEST-MIX-${createdAtMs}`;
  return false;
}
