const MODE_KEYS = ['jeopardy', 'attackDefense', 'koth'];

/** Validate the public combined-board normalization contract under k6 or Node. */
export function validCombinedBoard(model, minimumModes = 2) {
  if (!Array.isArray(model?.items) || model.items.length < 2 || typeof model?.fullySettled !== 'boolean') return false;
  const modes = MODE_KEYS.filter((key) => model?.modes?.[key]?.active === true);
  if (modes.length < minimumModes) return false;
  const expectedWeight = 1 / modes.length;
  if (!modes.every((key) => Math.abs(model.modes[key].weight - expectedWeight) < 1e-9)) return false;
  return model.items.every((item) => {
    if (
      !Number.isFinite(item?.score) ||
      item.score < 0 ||
      item.score > 100 ||
      !Number.isFinite(item?.projectedScore) ||
      item.projectedScore < 0 ||
      item.projectedScore > 100
    )
      return false;
    const settled = modes.map((key) => item?.components?.[key]?.score);
    const projected = modes.map((key) => item?.components?.[key]?.projectedScore);
    if (![...settled, ...projected].every((value) => Number.isFinite(value) && value >= 0 && value <= 100))
      return false;
    const settledMean = settled.reduce((sum, value) => sum + value, 0) / modes.length;
    const projectedMean = projected.reduce((sum, value) => sum + value, 0) / modes.length;
    return Math.abs(settledMean - item.score) < 0.0002 && Math.abs(projectedMean - item.projectedScore) < 0.0002;
  });
}
