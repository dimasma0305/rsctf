import { isAbsolute } from "node:path";

const SAFE_PARENT_KEYS = Object.freeze([
  "PATH",
  "SSL_CERT_FILE",
  "SSL_CERT_DIR",
  "LANG",
  "LC_ALL",
  "TZ",
]);
const SCENARIO_KEYS = new Set(["CHEAT_CONFIG"]);

/** Build the complete environment for the credential-bearing anti-cheat k6 child. */
export function cheatK6Environment(parentEnvironment, scenarioEnvironment, sandboxDirectory) {
  if (
    !parentEnvironment ||
    typeof parentEnvironment !== "object" ||
    !scenarioEnvironment ||
    typeof scenarioEnvironment !== "object" ||
    typeof sandboxDirectory !== "string" ||
    !isAbsolute(sandboxDirectory)
  ) {
    throw new Error("anti-cheat k6 environment requires objects and an absolute sandbox directory");
  }

  const environment = {};
  for (const key of SAFE_PARENT_KEYS) {
    const value = parentEnvironment[key];
    if (typeof value === "string" && value.length > 0) environment[key] = value;
  }
  environment.PATH ||= "/usr/local/bin:/usr/bin:/bin";
  // A private HOME prevents k6 from loading a user-level config containing
  // debug/output directives. Temporary files also stay inside the credential
  // directory that the runner removes after the child exits.
  environment.HOME = sandboxDirectory;
  environment.TMPDIR = sandboxDirectory;
  environment.TMP = sandboxDirectory;
  environment.TEMP = sandboxDirectory;

  for (const [key, value] of Object.entries(scenarioEnvironment)) {
    if (!SCENARIO_KEYS.has(key)) {
      throw new Error(`anti-cheat k6 environment rejects unexpected key ${key}`);
    }
    if (typeof value !== "string" || value.length === 0) {
      throw new Error(`anti-cheat k6 environment requires a non-empty ${key}`);
    }
    environment[key] = value;
  }
  if (!isAbsolute(environment.CHEAT_CONFIG || "")) {
    throw new Error("anti-cheat k6 environment requires an absolute CHEAT_CONFIG path");
  }

  return Object.freeze(environment);
}
