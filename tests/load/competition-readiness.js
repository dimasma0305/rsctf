// Pure validation for adopting the paused readiness round created by a
// realistic-competition provision. Keeping this independent of SQL/process
// orchestration makes the provision-to-lifecycle handoff contract testable.

function positiveInteger(value, label) {
  const number = Number(value);
  if (!Number.isSafeInteger(number) || number <= 0) {
    throw new Error(`invalid ${label}: ${value}`);
  }
  return number;
}

export function adoptPausedCompetitionReadiness({
  state,
  realisticCompetition,
  scoringPaused,
  fleetAdoptable,
  epoch,
  evidence,
  expectedServices,
}) {
  if (
    !realisticCompetition ||
    state?.scoringPausedAfterReadiness !== true ||
    scoringPaused !== true
  ) {
    return null;
  }

  const readinessRound = positiveInteger(state.readinessRound, "manifest readiness round");
  const serviceCount = positiveInteger(expectedServices, "expected readiness services");
  if (!fleetAdoptable) {
    throw new Error(
      "the provisioned scoring round is paused, but its verified BYOC fleet is no longer adoptable; reprovision the event",
    );
  }

  const observed = {
    epochRound: Number(epoch?.liveRound),
    evidenceRound: Number(evidence?.liveRound),
    requestedServices: Number(evidence?.requestedServices),
    plantedFlags: Number(evidence?.plantedFlags),
    deliveredFlags: Number(evidence?.deliveredFlags),
    verifiedFlags: Number(evidence?.verifiedFlags),
  };
  if (
    observed.epochRound !== readinessRound ||
    observed.evidenceRound !== readinessRound ||
    observed.requestedServices !== serviceCount ||
    observed.plantedFlags !== serviceCount ||
    observed.deliveredFlags !== serviceCount ||
    observed.verifiedFlags !== serviceCount
  ) {
    throw new Error(
      `paused competition readiness no longer matches its manifest: ${JSON.stringify({ readinessRound, ...observed })}`,
    );
  }

  return { ...evidence, liveRound: readinessRound };
}
