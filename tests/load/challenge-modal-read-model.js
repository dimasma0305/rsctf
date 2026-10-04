export const MAX_MODAL_SOLVERS = 20;
export const MAX_MODAL_SOLVER_BODY_BYTES = 64 * 1024;

export function validSolverPage(model, bodyBytes) {
  if (!model || typeof model !== "object" || Array.isArray(model)) return false;
  if (!Array.isArray(model.data) || model.data.length > MAX_MODAL_SOLVERS)
    return false;
  if (!Number.isSafeInteger(model.total) || model.total < model.data.length)
    return false;
  if (
    model.nextSkip !== null &&
    (!Number.isSafeInteger(model.nextSkip) ||
      model.nextSkip < model.data.length ||
      model.nextSkip > model.total)
  )
    return false;
  if (
    !Number.isSafeInteger(bodyBytes) ||
    bodyBytes > MAX_MODAL_SOLVER_BODY_BYTES
  )
    return false;

  return model.data.every(
    (solver) =>
      solver &&
      typeof solver.teamName === "string" &&
      solver.teamName.length > 0 &&
      (solver.teamAvatar === null || typeof solver.teamAvatar === "string") &&
      (solver.userName === null || typeof solver.userName === "string") &&
      typeof solver.type === "string" &&
      Number.isFinite(solver.time),
  );
}
