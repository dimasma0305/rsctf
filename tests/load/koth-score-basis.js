const EPSILON = 1e-6;

function validAverage(points, weight, average) {
  if (
    !Number.isFinite(points) ||
    points < 0 ||
    !Number.isFinite(weight) ||
    weight < 0 ||
    !Number.isFinite(average) ||
    average < 0 ||
    average > 100
  )
    return false;

  if (weight === 0) return points === 0 && average === 0;
  if (points > 100 * weight + EPSILON) return false;
  return Math.abs(points / weight - average) < EPSILON;
}

/** Validate the exact weighted basis behind both KotH event averages. */
export function validKothEventScoreBasis(team) {
  return (
    validAverage(team?.settledEpochPoints, team?.settledEpochWeight, team?.settledTotal) &&
    validAverage(team?.projectedEpochPoints, team?.projectedEpochWeight, team?.projectedTotal) &&
    team.projectedEpochWeight + EPSILON >= team.settledEpochWeight
  );
}
