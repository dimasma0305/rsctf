const EPSILON = 1e-6;
const MAX_FIELD_BEST_MULTIPLIER = 4;

function boundedScore(value) {
  return Number.isFinite(value) && value >= 0 && value <= 100 + EPSILON;
}

function boundedShare(value) {
  return Number.isFinite(value) && value >= 0 && value <= 1 + EPSILON;
}

function validMultiplier(value) {
  return Number.isFinite(value) && value >= 1 - EPSILON && value <= MAX_FIELD_BEST_MULTIPLIER + EPSILON;
}

/**
 * Validate the exact field-best basis behind both KotH event scores.
 *
 * Every hill publishes one capped field-best multiplier and one weight share
 * for the whole roster. A team's normalized hill score is its local average
 * scaled by that multiplier (clamped to 100), and the event score is the
 * share-weighted sum of the normalized hill scores.
 */
export function validKothEventScoreBasis(team, hills) {
  if (!boundedScore(team?.settledTotal) || !boundedScore(team?.projectedTotal)) return false;
  if (!Array.isArray(team?.hills) || !Array.isArray(hills)) return false;
  let settled = 0;
  let projected = 0;
  let settledShare = 0;
  let projectedShare = 0;
  for (const hill of hills) {
    if (!boundedShare(hill?.settledShare) || !boundedShare(hill?.projectedShare)) return false;
    if (!validMultiplier(hill?.settledMultiplier) || !validMultiplier(hill?.projectedMultiplier)) return false;
    if (!boundedScore(hill?.settledFieldBest) || !boundedScore(hill?.projectedFieldBest)) return false;
    const score = team.hills.find((cell) => cell?.challengeId === hill.challengeId);
    const settledNormalized = score ? score.settledNormalizedPoints : 0;
    const projectedNormalized = score ? score.projectedNormalizedPoints : 0;
    if (!boundedScore(settledNormalized) || !boundedScore(projectedNormalized)) return false;
    if (score) {
      if (!boundedScore(score.settledPoints) || !boundedScore(score.projectedPoints)) return false;
      if (score.settledPoints > hill.settledFieldBest + EPSILON) return false;
      const expectedSettled = Math.min(100, score.settledPoints * hill.settledMultiplier);
      const expectedProjected = Math.min(100, score.projectedPoints * hill.projectedMultiplier);
      if (Math.abs(expectedSettled - settledNormalized) > EPSILON) return false;
      if (Math.abs(expectedProjected - projectedNormalized) > EPSILON) return false;
    }
    settled += hill.settledShare * settledNormalized;
    projected += hill.projectedShare * projectedNormalized;
    settledShare += hill.settledShare;
    projectedShare += hill.projectedShare;
  }
  if (!boundedShare(settledShare) || !boundedShare(projectedShare)) return false;
  return Math.abs(settled - team.settledTotal) < EPSILON && Math.abs(projected - team.projectedTotal) < EPSILON;
}
