export function validVisibleChallengeProjection(model) {
  if (!model || typeof model !== "object" || Array.isArray(model)) return false;
  if (
    !model.challenges ||
    typeof model.challenges !== "object" ||
    Array.isArray(model.challenges)
  )
    return false;
  if (!Number.isSafeInteger(model.challengeCount) || model.challengeCount < 0)
    return false;

  const challenges = Object.values(model.challenges).flatMap((category) =>
    Array.isArray(category) ? category : [null],
  );
  if (
    challenges.some(
      (challenge) => !Number.isSafeInteger(challenge?.id) || challenge.id <= 0,
    ) ||
    new Set(challenges.map((challenge) => challenge.id)).size !==
      challenges.length ||
    model.challengeCount !== challenges.length
  ) {
    return false;
  }

  if (!model.rank || !Array.isArray(model.rank.solvedChallenges)) return false;
  if (
    !Number.isSafeInteger(model.rank.solvedCount) ||
    model.rank.solvedCount < 0
  )
    return false;
  if (model.rank.solvedCount !== model.rank.solvedChallenges.length)
    return false;

  const visibleIds = new Set(challenges.map((challenge) => challenge.id));
  return model.rank.solvedChallenges.every(
    (solve) => Number.isSafeInteger(solve?.id) && visibleIds.has(solve.id),
  );
}
