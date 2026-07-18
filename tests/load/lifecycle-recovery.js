function timestamp(value, label) {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new TypeError(`${label} must be a positive integer timestamp`);
  }
  return value;
}

function manifest(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("lifecycle recovery requires a manifest object");
  }
  return value;
}

export function abortedLifecycleState(state, error, atMs) {
  const current = manifest(state);
  const reason = String(error?.message ?? error).slice(0, 500);
  return Object.freeze({
    ...current,
    abortedAtMs: timestamp(atMs, "abort time"),
    abortReason: reason,
    simulationStatus: "aborted",
  });
}

export function cleanupFailureState(state, errors, runFailed, atMs) {
  const current = manifest(state);
  if (
    !Array.isArray(errors) ||
    errors.length === 0 ||
    errors.some((error) => typeof error !== "string")
  ) {
    throw new TypeError("cleanup recovery requires non-empty string errors");
  }
  if (typeof runFailed !== "boolean")
    throw new TypeError("runFailed must be boolean");
  return Object.freeze({
    ...current,
    cleanupFailedAtMs: timestamp(atMs, "cleanup failure time"),
    cleanupErrors: Object.freeze([...errors]),
    cleanupIncomplete: true,
    simulationStatus: runFailed ? "aborted" : "cleanup-failed",
  });
}

export function shouldResumeOfficialScoring(runFailed) {
  if (typeof runFailed !== "boolean")
    throw new TypeError("runFailed must be boolean");
  return !runFailed;
}

export function assertLifecycleRunClaimable(state) {
  const current = manifest(state);
  const status = current.simulationStatus;
  if (status == null || status === "running") return;
  throw new Error(
    `lifecycle manifest is terminal (${String(status)}); provision a new tagged event instead`,
  );
}
