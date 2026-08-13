const MODE_KEYS = ["jeopardy", "attackDefense", "koth"];

/** Validate the public combined-board normalization contract under k6 or Node. */
export function validCombinedBoard(model, minimumModes = 2) {
  if (
    !Array.isArray(model?.items) ||
    model.items.length < 2 ||
    typeof model?.fullySettled !== "boolean"
  )
    return false;
  const modes = MODE_KEYS.filter((key) => model?.modes?.[key]?.active === true);
  if (modes.length < minimumModes) return false;
  const challengeCounts = [];
  let totalChallenges = 0;
  for (const key of MODE_KEYS) {
    const mode = model?.modes?.[key];
    if (mode?.active === true) {
      if (!Number.isSafeInteger(mode.challengeCount) || mode.challengeCount <= 0)
        return false;
      challengeCounts.push(mode.challengeCount);
      totalChallenges += mode.challengeCount;
    } else if (mode?.challengeCount !== 0 || mode?.weight !== 0) {
      return false;
    }
  }
  for (let index = 0; index < modes.length; index += 1) {
    if (
      Math.abs(
        model.modes[modes[index]].weight -
          challengeCounts[index] / totalChallenges,
      ) >= 1e-9
    )
      return false;
  }

  // This validator runs on every sampled board. Keep it allocation-free per
  // team so strict semantic checking cannot become the load generator's
  // bottleneck when a fixture contains thousands of participants.
  for (const item of model.items) {
    if (
      !Number.isFinite(item?.score) ||
      item.score < 0 ||
      item.score > 100 ||
      !Number.isFinite(item?.projectedScore) ||
      item.projectedScore < 0 ||
      item.projectedScore > 100
    )
      return false;
    let settledTotal = 0;
    let projectedTotal = 0;
    for (let index = 0; index < modes.length; index += 1) {
      const component = item?.components?.[modes[index]];
      const settled = component?.score;
      const projected = component?.projectedScore;
      if (
        !Number.isFinite(settled) ||
        settled < 0 ||
        settled > 100 ||
        !Number.isFinite(projected) ||
        projected < 0 ||
        projected > 100
      )
        return false;
      settledTotal += settled * challengeCounts[index];
      projectedTotal += projected * challengeCounts[index];
    }
    if (
      Math.abs(settledTotal / totalChallenges - item.score) >= 0.0002 ||
      Math.abs(projectedTotal / totalChallenges - item.projectedScore) >= 0.0002
    )
      return false;
  }
  return true;
}
