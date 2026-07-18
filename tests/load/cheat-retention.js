// Retention policy shared by standalone and lifecycle-embedded anti-cheat drills.
// Keeping it pure makes the cleanup contract testable without provisioning an event.

export function cheatRetentionPolicy(environment = {}) {
  const integrated = environment.RSCTF_INTEGRATED_CHEAT_CHILD === "1";
  return Object.freeze({
    integrated,
    retainNamespace: !integrated || environment.RETAIN_EVENT === "1",
  });
}

export function inheritedCheatOrchestrationToken(environment = {}, policy) {
  const resolvedPolicy = policy ?? cheatRetentionPolicy(environment);
  if (!resolvedPolicy || typeof resolvedPolicy.integrated !== "boolean") {
    throw new Error("cheat simulation retention policy is invalid");
  }
  if (!resolvedPolicy.integrated) return null;
  const token = environment.RSCTF_LOAD_ORCHESTRATION_LOCK_TOKEN;
  if (typeof token !== "string" || token.length < 16) {
    throw new Error("embedded cheat mode requires the lifecycle parent's process-lock token");
  }
  return token;
}

export function recordCheatSimulation(state, simulation, policy) {
  if (!state || typeof state !== "object" || !simulation || typeof simulation !== "object") {
    throw new Error("cheat simulation state and evidence must be objects");
  }
  if (!policy || typeof policy.retainNamespace !== "boolean") {
    throw new Error("cheat simulation retention policy is invalid");
  }

  // Never remove protection from an already-retained namespace. An embedded drill
  // only refrains from adding protection when its lifecycle parent did not request it.
  const retained = state.retained === true || policy.retainNamespace;
  return {
    ...state,
    ...(retained ? { retained: true } : {}),
    cheatSimulation: {
      ...state.cheatSimulation,
      ...simulation,
      retained,
    },
  };
}
