import assert from "node:assert/strict";
import test from "node:test";

import {
  dockerLabelArgs,
  dockerOwnershipFilterArgs,
  dockerTeamClientFilterArgs,
  fleetParticipantBindings,
  fleetLabelKeys,
  fleetLabels,
  fleetOwner,
  normalizeFleetScope,
  normalizeTeamClientScope,
  ownsFleetResource,
  ownsTeamClient,
  selectTeamClientOwnershipRecord,
  sameTeamClientScope,
  teamClientLabelKeys,
  teamClientLabels,
  teamClientOwner,
} from "../fleet-ownership.js";

test("fleet resources are bound to one game, challenge, role, and participation", () => {
  const scope = normalizeFleetScope(41, 73, [101, 102]);
  const labels = fleetLabels(scope, "relay", 101);

  assert.deepEqual(labels, {
    [fleetLabelKeys.owner]: fleetOwner,
    [fleetLabelKeys.game]: "41",
    [fleetLabelKeys.challenge]: "73",
    [fleetLabelKeys.role]: "relay",
    [fleetLabelKeys.participation]: "101",
  });
  assert.equal(ownsFleetResource(labels, scope, "relay", 101), true);
  assert.equal(
    ownsFleetResource(labels, normalizeFleetScope(42, 73, [101]), "relay", 101),
    false,
  );
  assert.equal(
    ownsFleetResource(labels, scope, "isolated-service", 101),
    false,
  );
  assert.equal(ownsFleetResource(labels, scope, "relay", 102), false);
});

test("cleanup filters always include the lifecycle owner and exact game", () => {
  const gameScope = normalizeFleetScope(41);
  const challengeScope = normalizeFleetScope(41, 73);

  assert.deepEqual(dockerOwnershipFilterArgs(gameScope), [
    "--filter",
    `label=${fleetLabelKeys.owner}=${fleetOwner}`,
    "--filter",
    `label=${fleetLabelKeys.game}=41`,
  ]);
  assert.deepEqual(dockerOwnershipFilterArgs(challengeScope), [
    ...dockerOwnershipFilterArgs(gameScope),
    "--filter",
    `label=${fleetLabelKeys.challenge}=73`,
  ]);
  assert.deepEqual(
    dockerLabelArgs(fleetLabels(challengeScope, "shared-service")),
    [
      "--label",
      `${fleetLabelKeys.owner}=${fleetOwner}`,
      "--label",
      `${fleetLabelKeys.game}=41`,
      "--label",
      `${fleetLabelKeys.challenge}=73`,
      "--label",
      `${fleetLabelKeys.role}=shared-service`,
    ],
  );
});

test("fleet ownership rejects ambiguous or out-of-scope identities", () => {
  assert.throws(() => normalizeFleetScope(0, 2), /game id/);
  assert.throws(() => normalizeFleetScope(1, 2, [9, 9]), /distinct/);
  const scope = normalizeFleetScope(1, 2, [9]);
  assert.throws(() => fleetLabels(scope, "relay", 10), /outside/);
  assert.throws(
    () => fleetLabels(normalizeFleetScope(1), "relay", 9),
    /challenge id/,
  );
});

test("teardown bindings come from every removed resource instead of a caller prefix", () => {
  const fullScope = normalizeFleetScope(41, 73, [101, 102]);
  const callerPrefix = normalizeFleetScope(41, 73, [101]);
  assert.deepEqual(
    fleetParticipantBindings(callerPrefix, [
      { kind: "container", labels: fleetLabels(fullScope, "shared-service") },
      { kind: "container", labels: fleetLabels(fullScope, "relay", 101) },
      {
        kind: "container",
        labels: fleetLabels(fullScope, "isolated-service", 102),
      },
      { kind: "volume", labels: fleetLabels(fullScope, "flag-volume", 102) },
    ]),
    [
      { challengeId: 73, participationId: 101 },
      { challengeId: 73, participationId: 102 },
    ],
  );
});

test("team clients are bound to one run and one indexed participation", () => {
  const scope = normalizeTeamClientScope(
    94,
    "competitive-v2-final-100",
    [6166, 6167],
  );
  const labels = teamClientLabels(scope, 6167, 1);

  assert.deepEqual(labels, {
    [teamClientLabelKeys.owner]: teamClientOwner,
    [teamClientLabelKeys.game]: "94",
    [teamClientLabelKeys.run]: "competitive-v2-final-100",
    [teamClientLabelKeys.role]: "team-client",
    [teamClientLabelKeys.participation]: "6167",
    [teamClientLabelKeys.index]: "1",
  });
  assert.equal(ownsTeamClient(labels, scope, 6167, 1), true);
  assert.equal(
    ownsTeamClient(
      labels,
      normalizeTeamClientScope(94, "another-run", [6166, 6167]),
      6167,
      1,
    ),
    false,
  );
  assert.throws(
    () => teamClientLabels(scope, 6166, 1),
    /bound to participation/,
  );
});

test("team-client cleanup filters include the exact owner, game, run, and role", () => {
  const scope = normalizeTeamClientScope(94, "run-123", [1, 2]);
  assert.deepEqual(dockerTeamClientFilterArgs(scope), [
    "--filter",
    `label=${teamClientLabelKeys.owner}=${teamClientOwner}`,
    "--filter",
    `label=${teamClientLabelKeys.game}=94`,
    "--filter",
    `label=${teamClientLabelKeys.run}=run-123`,
    "--filter",
    `label=${teamClientLabelKeys.role}=team-client`,
  ]);
});

test("team-client scopes reject ambiguous ownership", () => {
  assert.throws(() => normalizeTeamClientScope(1, "bad run", [1, 2]), /run id/);
  assert.throws(() => normalizeTeamClientScope(1, "run", [1]), /at least two/);
  assert.throws(() => normalizeTeamClientScope(1, "run", [1, 1]), /distinct/);
});

test("normal lifecycle manifests without live client ownership are safe no-ops", () => {
  const direct = { schemaVersion: 1, owner: teamClientOwner };
  assert.equal(selectTeamClientOwnershipRecord(direct), direct);
  assert.equal(
    selectTeamClientOwnershipRecord({
      schemaVersion: 1,
      teamClientOwnership: null,
    }),
    null,
  );
  assert.equal(selectTeamClientOwnershipRecord({ schemaVersion: 1 }), null);
  assert.deepEqual(
    selectTeamClientOwnershipRecord({ teamClientOwnership: direct }),
    direct,
  );
});

test("persisted team-client ownership cannot cross a run boundary", () => {
  const current = normalizeTeamClientScope(94, "current-run", [1, 2]);
  assert.equal(sameTeamClientScope(current, current), true);
  assert.equal(
    sameTeamClientScope(
      current,
      normalizeTeamClientScope(94, "stale-run", [1, 2]),
    ),
    false,
  );
  assert.equal(
    sameTeamClientScope(
      current,
      normalizeTeamClientScope(95, "current-run", [1, 2]),
    ),
    false,
  );
  assert.equal(
    sameTeamClientScope(
      current,
      normalizeTeamClientScope(94, "current-run", [2, 1]),
    ),
    false,
  );
});
