import { isImmutableImageReference } from './fixture-image-config.js';

export const byocFixtureLabelKeys = Object.freeze({
  owner: 'rsctf.load.byoc.owner',
  run: 'rsctf.load.byoc.run',
  role: 'rsctf.load.byoc.role',
  index: 'rsctf.load.byoc.index',
  participation: 'rsctf.load.byoc.participation',
  challenge: 'rsctf.load.byoc.challenge',
});

export const byocFixtureOwner = 'byoc-stress-v1';

function positiveInteger(value, label) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(`${label} must be a positive integer (got ${value})`);
  }
  return parsed;
}

export function normalizeByocRunId(value) {
  const runId = String(value || '').trim();
  if (!/^[a-z0-9][a-z0-9-]{0,47}$/.test(runId)) {
    throw new Error(
      `RSCTF_BYOC_RUN_ID must be a lowercase 1-48 character DNS-safe identifier (got ${value})`,
    );
  }
  return runId;
}

export function byocFixtureNames(runId) {
  const run = normalizeByocRunId(runId);
  return Object.freeze({
    service: `load_svc_${run}`,
    agent(index) {
      const position = Number(index);
      if (!Number.isSafeInteger(position) || position < 0) {
        throw new Error(`BYOC agent index must be a non-negative integer (got ${index})`);
      }
      return `load_agent_${run}_${position}`;
    },
  });
}

export function byocFixtureLabels(runId, role, details = {}) {
  const run = normalizeByocRunId(runId);
  if (!['shared-service', 'relay'].includes(role)) {
    throw new Error(`unsupported BYOC fixture role ${role}`);
  }
  const labels = {
    [byocFixtureLabelKeys.owner]: byocFixtureOwner,
    [byocFixtureLabelKeys.run]: run,
    [byocFixtureLabelKeys.role]: role,
  };
  if (role === 'relay') {
    const index = Number(details.index);
    if (!Number.isSafeInteger(index) || index < 0) {
      throw new Error(`BYOC relay index must be a non-negative integer (got ${details.index})`);
    }
    labels[byocFixtureLabelKeys.index] = String(index);
    labels[byocFixtureLabelKeys.participation] = String(
      positiveInteger(details.participationId, 'BYOC relay participation id'),
    );
    labels[byocFixtureLabelKeys.challenge] = String(
      positiveInteger(details.challengeId, 'BYOC relay challenge id'),
    );
  }
  return Object.freeze(labels);
}

export function dockerByocLabelArgs(labels) {
  return Object.entries(labels).flatMap(([key, value]) => ['--label', `${key}=${value}`]);
}

export function dockerByocRunFilterArgs(runId) {
  const run = normalizeByocRunId(runId);
  return [
    '--filter',
    `label=${byocFixtureLabelKeys.owner}=${byocFixtureOwner}`,
    '--filter',
    `label=${byocFixtureLabelKeys.run}=${run}`,
  ];
}

export function assertOwnedByocContainer(resource, runId) {
  if (!resource || typeof resource !== 'object') {
    throw new Error('BYOC fixture container inspection is missing');
  }
  const run = normalizeByocRunId(runId);
  const labels = resource.Config?.Labels || {};
  if (
    labels[byocFixtureLabelKeys.owner] !== byocFixtureOwner ||
    labels[byocFixtureLabelKeys.run] !== run
  ) {
    throw new Error('refusing to remove a container outside the exact BYOC run ownership scope');
  }
  const role = labels[byocFixtureLabelKeys.role];
  const names = byocFixtureNames(run);
  const name = String(resource.Name || '').replace(/^\//, '');
  const id = String(resource.Id || '');
  if (!/^[a-f0-9]{12,64}$/.test(id)) {
    throw new Error(`owned BYOC container ${name || '<missing>'} has an invalid Docker id`);
  }
  if (role === 'shared-service') {
    if (name !== names.service) {
      throw new Error(`owned BYOC shared service has unexpected name ${name || '<missing>'}`);
    }
  } else if (role === 'relay') {
    const index = Number(labels[byocFixtureLabelKeys.index]);
    positiveInteger(labels[byocFixtureLabelKeys.participation], 'owned BYOC participation label');
    positiveInteger(labels[byocFixtureLabelKeys.challenge], 'owned BYOC challenge label');
    if (!Number.isSafeInteger(index) || index < 0 || name !== names.agent(index)) {
      throw new Error(`owned BYOC relay has inconsistent name/index labels (${name || '<missing>'})`);
    }
  } else {
    throw new Error(`owned BYOC container has unsupported role ${role || '<missing>'}`);
  }
  return Object.freeze({ id, name, role });
}

export function assertByocFixtureImages({ agentImage, serviceImage, reportable }) {
  const agent = String(agentImage || '').trim();
  const service = String(serviceImage || '').trim();
  if (!agent) throw new Error('RSCTF_BYOC_AGENT_IMAGE must not be empty');
  if (!service) throw new Error('RSCTF_BYOC_SERVICE_IMAGE must not be empty');
  if (reportable && !isImmutableImageReference(agent)) {
    throw new Error(
      'RSCTF_ACCEPTANCE_REPORTABLE=1 requires RSCTF_BYOC_AGENT_IMAGE to be an immutable repository digest or image ID',
    );
  }
  if (reportable && !isImmutableImageReference(service)) {
    throw new Error(
      'RSCTF_ACCEPTANCE_REPORTABLE=1 requires RSCTF_BYOC_SERVICE_IMAGE to be an immutable repository digest or image ID',
    );
  }
  return Object.freeze({ agentImage: agent, serviceImage: service, reportable: Boolean(reportable) });
}

export function assertExactTunnelCount(expected, actual, label = 'BYOC tunnel count') {
  const required = positiveInteger(expected, 'expected BYOC tunnel count');
  const observed = Number(actual);
  if (!Number.isSafeInteger(observed) || observed < 0) {
    throw new Error(`${label} is invalid (got ${actual})`);
  }
  if (observed !== required) {
    throw new Error(`${label} must equal ${required}, observed ${observed}`);
  }
  return observed;
}

function safeContainerName(value, label) {
  const name = String(value || '').trim();
  if (!/^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/.test(name)) {
    throw new Error(`${label} must be one exact Docker container name`);
  }
  return name;
}

/** Validate the one web replica that worst-case.mjs is authorized to restart. */
export function assertByocRestartTarget(environment, resource) {
  const name = safeContainerName(environment.RSCTF_CONTAINER, 'RSCTF_CONTAINER');
  if (String(environment.CONFIRM_RSCTF_RESTART || '').trim() !== name) {
    throw new Error(`set CONFIRM_RSCTF_RESTART=${name} to acknowledge the exact disposable replica`);
  }
  if (!resource || typeof resource !== 'object') {
    throw new Error(`Docker inspection for ${name} is missing`);
  }
  const actualName = String(resource.Name || '').replace(/^\//, '');
  const labels = resource.Config?.Labels || {};
  const project = String(labels['com.docker.compose.project'] || '').trim();
  const service = String(labels['com.docker.compose.service'] || '').trim();
  if (actualName !== name || !resource.State?.Running) {
    throw new Error(`${name} is not the inspected running Docker container`);
  }
  if (!project || service !== 'rsctf') {
    throw new Error(`${name} must be a Compose rsctf web replica, not ${service || 'an unlabeled container'}`);
  }

  const configuredProject = String(environment.COMPOSE_PROJECT_NAME || '').trim();
  if (configuredProject && configuredProject !== project) {
    throw new Error(
      `${name} belongs to Compose project ${project}, not COMPOSE_PROJECT_NAME=${configuredProject}`,
    );
  }

  const reportable = environment.RSCTF_ACCEPTANCE_REPORTABLE === '1';
  if (reportable) {
    if (!configuredProject) {
      throw new Error('RSCTF_ACCEPTANCE_REPORTABLE=1 requires COMPOSE_PROJECT_NAME');
    }
    const marker = String(environment.ADMIN_LIFECYCLE_STACK_MARKER || '').trim();
    if (!/^[A-Za-z0-9][A-Za-z0-9._-]{7,127}$/.test(marker)) {
      throw new Error(
        'RSCTF_ACCEPTANCE_REPORTABLE=1 requires ADMIN_LIFECYCLE_STACK_MARKER for the disposable stack',
      );
    }
    const markerEntries = (resource.Config?.Env || []).filter((entry) =>
      String(entry).startsWith('RSCTF_ADMIN_LIFECYCLE_MARKER='),
    );
    if (
      markerEntries.length !== 1 ||
      markerEntries[0] !== `RSCTF_ADMIN_LIFECYCLE_MARKER=${marker}`
    ) {
      throw new Error(`${name} does not carry the one exact disposable-stack marker`);
    }
  }

  const startedAt = String(resource.State?.StartedAt || '').trim();
  const id = String(resource.Id || '');
  if (!/^[a-f0-9]{12,64}$/.test(id) || !startedAt) {
    throw new Error(`${name} has incomplete Docker identity/start evidence`);
  }
  return Object.freeze({ id, name, project, service, startedAt, reportable });
}
