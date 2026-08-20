import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(new URL("../cheat-acceptance.mjs", import.meta.url), "utf8");
const retainedSource = readFileSync(new URL("../cheat-event.mjs", import.meta.url), "utf8");

test("compact acceptance wires its isolated shared-network journeys", () => {
  assert.match(source, /async function exerciseSharedIpLogins\(/);
  assert.equal((source.match(/await exerciseSharedIpLogins\(/g) || []).length, 4);
  assert.match(source, /await exerciseDistinctIpLogins\(gameId, cohort,/);
  assert.doesNotMatch(source, /\bexerciseNat\b/);
  assert.match(source, /"X-Forwarded-For": ip/);
  assert.match(source, /"X-Forwarded-For": subject\.ip/);
  assert.match(source, /X-Real-IP negative login/);
  assert.match(source, /startHeldSharedIpLogin\(/);
  assert.match(source, /pg_stat_activity/);
  assert.match(source, /query LIKE 'WITH contexts AS MATERIALIZED \(%'/);
  assert.doesNotMatch(source, /query LIKE '%INSERT INTO \"IdentityObservations\"%'/);
  assert.match(source, /cheat_acceptance_hold_identity/);
  assert.match(source, /exerciseFinalizationGraceControl\(/);
  assert.match(source, /finalization-grace reconciler phase/);
  assert.match(source, /phase-aligned finalization-grace cycle/);
  assert.match(source, /sentinel\.dbNowMicros/);
  assert.match(source, /sealedAtMicros/);
  assert.match(source, /final-only network correlation ran before finalization grace completed/);
  assert.match(source, /assertPostEndLoginExcluded\(/);
  assert.match(source, /rsctf\.identity_neutral_insert/);
  assert.match(source, /account insert lacks same-transaction identity adjudication/);
  assert.match(source, /exerciseIdentityAwareTeamAccept\(/);
  assert.match(source, /fingerprintProof: proof/);
  assert.match(source, /first_download_at_submit/);
  assert.match(source, /first_container_start_at_submit/);
  assert.match(source, /downloadDeltaMs < 120_000/);
  assert.match(source, /containerStartDeltaMs < 120_000/);
  assert.match(source, /TELEMETRY_ONLY_KINDS = new Set\(\[12, 13, 14, 21, 22, 28, 29, 31\]\)/);
  assert.match(source, /assertNoTelemetryOnlyEvents\(rows\)/);
  assert.match(source, /function assertHoneypotTelemetry\(/);
  assert.match(source, /rsctf-cheat-acceptance\/\$\{now\}/);
  assert.match(source, /game_id IS NULL/);
  assert.match(source, /row\[3\] === null/);
  assert.match(source, /raw honeypot telemetry appeared as a scored report row/);
  assert.match(source, /abnormalSolves\.length !== 0/);
  assert.match(source, /'lastReconciledAt',last_reconciled_at_utc/);
  assert.match(source, /CHEAT_ACCEPTANCE_ISOLATED/);
});

test("retained cheat drill provisions helper identities through the neutral marker", () => {
  assert.equal(
    (retainedSource.match(/WITH neutral_provisioning AS MATERIALIZED/g) || []).length,
    2,
  );
  assert.equal(
    (retainedSource.match(/set_config\('rsctf\.identity_neutral_insert','1',true\)/g) || []).length,
    2,
  );
  assert.equal((retainedSource.match(/CROSS JOIN neutral_provisioning/g) || []).length, 2);
  assert.match(
    retainedSource,
    /CONTEXT_KINDS = \[1, 2, 3, 4, 5, 6, 10, 11, 12, 13, 14, 21, 22, 23, 26, 28, 29, 31, 32, 36, 37\]/,
  );
  assert.match(retainedSource, /function assertHoneypotTelemetry\(/);
  assert.match(retainedSource, /rsctf-cheat-drill\/\$\{runId\}/);
  assert.match(retainedSource, /honeypot outbox absent/);
  assert.match(retainedSource, /honeypot suspicion absent/);
});
