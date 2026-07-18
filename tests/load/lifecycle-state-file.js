const DEFAULT_LIFECYCLE_STATE_BASENAME = ".lifecycle-state.json";
const LIFECYCLE_STATE_BASENAME_PATTERN =
  /^\.lifecycle-state(?:-[a-z0-9][a-z0-9-]{0,31})?\.json$/;

/**
 * Validate the only manifest names k6 may open. Keeping this as a basename
 * prevents a caller-controlled environment value from escaping tests/load/.
 */
export function lifecycleStateBasename(value) {
  const candidate =
    value === undefined || value === null
      ? DEFAULT_LIFECYCLE_STATE_BASENAME
      : value;
  if (
    typeof candidate !== "string" ||
    !LIFECYCLE_STATE_BASENAME_PATTERN.test(candidate)
  ) {
    throw new Error(
      "LIFECYCLE_STATE_FILE must be a valid lifecycle manifest basename",
    );
  }
  return candidate;
}

/** Derive the validated basename that the Node orchestrator passes to k6. */
export function lifecycleStateBasenameFromPath(path) {
  if (typeof path !== "string" || path.length === 0) {
    throw new Error("lifecycle state path must be a non-empty string");
  }
  const separator = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return lifecycleStateBasename(path.slice(separator + 1));
}

/** k6/lifecycle.js lives one directory below the manifest. */
export function lifecycleStateOpenPath(value) {
  return `../${lifecycleStateBasename(value)}`;
}
