function exactPositiveIntegerSet(values, label, { allowEmpty = false } = {}) {
  if (!Array.isArray(values) || (!allowEmpty && values.length === 0)) {
    throw new Error(`${label} must be ${allowEmpty ? "an" : "a non-empty"} array`);
  }
  const parsed = values.map((value) => {
    const number = Number(value);
    if (!Number.isSafeInteger(number) || number <= 0) {
      throw new Error(`${label} contains an invalid participation id`);
    }
    return number;
  });
  if (new Set(parsed).size !== parsed.length) {
    throw new Error(`${label} must contain distinct participation ids`);
  }
  return parsed;
}

/**
 * Select fresh detector actors, then freeze every other roster member as a
 * control. Prior ordinary-play evidence never removes a non-offender from the
 * control cohort; only evidence created after the drill baseline is evaluated.
 */
export function freezeCheatCohort(
  participationIds,
  actionableParticipationIds,
  offenderCount,
) {
  const roster = exactPositiveIntegerSet(participationIds, "anti-cheat roster");
  const actionable = exactPositiveIntegerSet(
    actionableParticipationIds,
    "actionable anti-cheat roster",
    { allowEmpty: true },
  );
  if (!Number.isSafeInteger(offenderCount) || offenderCount < 1) {
    throw new Error("anti-cheat offender count must be a positive integer");
  }
  if (roster.length <= offenderCount) {
    throw new Error("anti-cheat roster must contain at least one clean control");
  }

  const rosterSet = new Set(roster);
  if (actionable.some((participationId) => !rosterSet.has(participationId))) {
    throw new Error("actionable anti-cheat roster contains an unknown participation");
  }
  const actionableSet = new Set(actionable);
  const offenderIndices = roster
    .map((participationId, index) => ({ participationId, index }))
    .filter(({ participationId }) => !actionableSet.has(participationId))
    .slice(0, offenderCount)
    .map(({ index }) => index);
  if (offenderIndices.length !== offenderCount) {
    throw new Error(
      `anti-cheat drill needs ${offenderCount} actors without prior actionable evidence; ` +
        `only ${offenderIndices.length} are available`,
    );
  }

  const offenderIndexSet = new Set(offenderIndices);
  const cleanIndices = roster
    .map((_, index) => index)
    .filter((index) => !offenderIndexSet.has(index));
  if (cleanIndices.length !== roster.length - offenderCount) {
    throw new Error("anti-cheat cohort partition is incomplete");
  }
  return Object.freeze({
    offenderIndices: Object.freeze(offenderIndices),
    cleanIndices: Object.freeze(cleanIndices),
  });
}
