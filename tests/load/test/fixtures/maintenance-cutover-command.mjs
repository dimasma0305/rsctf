#!/usr/bin/env node

import { basename } from 'node:path';
import { readFileSync, writeFileSync } from 'node:fs';

const statePath = process.env.RSCTF_CUTOVER_TEST_STATE;
if (!statePath) {
  process.stderr.write('RSCTF_CUTOVER_TEST_STATE is required\n');
  process.exit(90);
}

const state = JSON.parse(readFileSync(statePath, 'utf8'));
const command = basename(process.argv[1]);
const args = process.argv.slice(2);

const save = () => writeFileSync(statePath, `${JSON.stringify(state, null, 2)}\n`);
const event = (value) => {
  state.events ??= [];
  state.events.push(value);
  save();
};
const fail = (message, status = 1) => {
  process.stderr.write(`${message}\n`);
  save();
  process.exit(status);
};
const output = (value) => process.stdout.write(typeof value === 'string' ? value : JSON.stringify(value));
const optionValue = (values, name) => {
  const index = values.indexOf(name);
  return index >= 0 ? values[index + 1] : undefined;
};
const setting = (values, name) => {
  const prefix = `${name}=`;
  return values.find((value) => value.startsWith(prefix))?.slice(prefix.length);
};
const runtimeRoles = new Set(['all', 'web', 'control', 'engine', 'network']);

function deploymentDocument(deployment) {
  const metadataLabels = {
    'app.kubernetes.io/name': state.appName,
    'app.kubernetes.io/instance': deployment.release,
    'app.kubernetes.io/managed-by': 'Helm',
  };
  if (!state.oldChartLabels) {
    metadataLabels['app.kubernetes.io/component'] = deployment.role;
  }
  return {
    metadata: { name: deployment.name, labels: metadataLabels },
    spec: {
      replicas: deployment.replicas,
      template: {
        metadata: {
          labels: {
            'app.kubernetes.io/name': state.appName,
            'app.kubernetes.io/instance': deployment.release,
            'app.kubernetes.io/component': deployment.role,
          },
        },
        spec: { containers: [{ name: 'rsctf', image: deployment.image }] },
      },
    },
    status: {
      replicas: deployment.statusReplicas,
      readyReplicas: deployment.statusReplicas,
      availableReplicas: deployment.statusReplicas,
      updatedReplicas: deployment.statusReplicas,
    },
  };
}

function runKubectl() {
  const values = [...args];
  if (values[0] === '-n' || values[0] === '--namespace') values.splice(0, 2);
  const verb = values[0];
  const resource = values[1];

  if (verb === 'get' && resource === 'deployments') {
    output({ items: state.deployments.map(deploymentDocument) });
    return;
  }
  if (verb === 'get' && resource === 'horizontalpodautoscalers.autoscaling') {
    output({
      items: (state.hpas ?? []).map((name) => ({
        metadata: { name: `hpa-${name}` },
        spec: { scaleTargetRef: { kind: 'Deployment', name } },
      })),
    });
    return;
  }
  if (verb === 'scale' && resource?.startsWith('deployment/')) {
    const name = resource.slice('deployment/'.length);
    const deployment = state.deployments.find((item) => item.name === name);
    if (!deployment) fail(`unknown deployment ${name}`);
    deployment.replicas = 0;
    if (!state.scaleTimeout) deployment.statusReplicas = 0;
    event(`k8s:scale:${name}:0`);
    return;
  }
  if (verb === 'get' && resource === 'pods') {
    const hasPods = state.scaleTimeout || state.deployments.some((item) => item.statusReplicas > 0);
    output({ items: hasPods ? [{ metadata: { name: 'old-runtime-pod' } }] : [] });
    return;
  }
  if (verb === 'get' && resource === 'jobs') {
    output({ items: state.jobs ?? [] });
    return;
  }
  if (verb === 'delete' && resource?.startsWith('job/')) {
    const name = resource.slice('job/'.length);
    const before = state.jobs?.length ?? 0;
    state.jobs = (state.jobs ?? []).filter((item) => item.metadata?.name !== name);
    if (state.jobs.length === before) fail(`unknown job ${name}`);
    event(`k8s:delete-job:${name}`);
    return;
  }
  if (verb === 'logs') {
    output('migration log inspected\n');
    return;
  }
  if (verb === 'get' && resource?.startsWith('deployment/')) {
    const name = resource.slice('deployment/'.length);
    const deployment = state.deployments.find((item) => item.name === name);
    if (!deployment) fail(`unknown deployment ${name}`);
    output(deployment.image);
    return;
  }
  fail(`unsupported kubectl invocation: ${values.join(' ')}`, 91);
}

function jobDocument(name, image, status) {
  return {
    metadata: { name },
    spec: { template: { spec: { containers: [{ name: 'migrate', image }] } } },
    status,
  };
}

function runHelm() {
  const getIndex = args.indexOf('get');
  if (getIndex >= 0 && args[getIndex + 1] === 'values') {
    const release = args[getIndex + 2];
    const deployment = state.deployments.find((item) => item.release === release);
    if (!deployment) fail(`unknown Helm release ${release}`);
    output({ runtimeRole: deployment.role, replicaCount: deployment.storedReplicas });
    return;
  }

  const upgradeIndex = args.indexOf('upgrade');
  if (upgradeIndex < 0) fail(`unsupported helm invocation: ${args.join(' ')}`, 92);
  const release = args[upgradeIndex + 1] === '--install'
    ? args[upgradeIndex + 2]
    : args[upgradeIndex + 1];
  const role = setting(args, 'runtimeRole');
  const repository = setting(args, 'image.repository');
  const digest = setting(args, 'image.digest');
  const image = `${repository}@${digest}`;
  if (role === 'migrate') {
    const quiesced = state.deployments.every(
      (item) => item.replicas === 0 && item.statusReplicas === 0,
    );
    if (!quiesced || state.scaleTimeout) fail('migration invoked before runtime quiescence', 93);
    if (!args.includes('--wait') || !args.includes('--wait-for-jobs')) {
      fail('migration was not synchronously health-gated', 94);
    }
    event(`helm:migrate:${release}:wait-for-jobs`);
    const name = `${release}-rsctf-migrate-test`;
    if (state.migrationFail) {
      state.jobs = [jobDocument(name, image, { failed: 1, active: 0, succeeded: 0 })];
      save();
      process.exit(1);
    }
    state.jobs = [jobDocument(name, image, { failed: 0, active: 0, succeeded: 1 })];
    state.migrationSucceeded = true;
    if (state.reappearAfterMigrationK8s) {
      state.deployments[0].replicas = state.deployments[0].storedReplicas;
    }
    save();
    return;
  }

  if (!state.migrationSucceeded) fail('runtime upgraded before successful migration', 95);
  const deployment = state.deployments.find((item) => item.release === release);
  if (!deployment) fail(`unknown runtime Helm release ${release}`);
  if (state.requireProviderOrder) {
    const isReadyAtCurrentDigest = (candidate) => (
      candidate?.image === image && candidate.statusReplicas > 0
    );
    if (deployment.role === 'engine') {
      const network = state.deployments.find((item) => item.role === 'network');
      if (network && !isReadyAtCurrentDigest(network)) {
        fail('engine Helm readiness failed before the network provider was restored', 96);
      }
    }
    if (deployment.role === 'web') {
      const control = state.deployments.find((item) => item.role === 'control');
      const engine = state.deployments.find((item) => item.role === 'engine');
      const network = state.deployments.find((item) => item.role === 'network');
      const providersReady = control
        ? isReadyAtCurrentDigest(control)
        : isReadyAtCurrentDigest(engine) && isReadyAtCurrentDigest(network);
      if (!providersReady) {
        fail('web Helm readiness failed before its providers were restored', 96);
      }
    }
  }
  const replicas = Number(setting(args, 'replicaCount'));
  if (!Number.isSafeInteger(replicas) || replicas < 1) fail('invalid runtime replica count');
  deployment.replicas = replicas;
  deployment.statusReplicas = replicas;
  deployment.storedReplicas = replicas;
  deployment.image = image;
  event(`helm:runtime:${release}:${replicas}:${image}`);
}

function containerMatches(container, values) {
  if (!values.includes('--all') && !container.running) return false;
  const filters = [];
  for (let index = 0; index < values.length; index += 1) {
    if (values[index] === '--filter') filters.push(values[index + 1]);
  }
  return filters.every((filter) => {
    const expected = filter.slice('label='.length);
    if (filter.startsWith('label=com.docker.compose.project=')) {
      return container.project === expected.slice('com.docker.compose.project='.length);
    }
    if (filter.startsWith('label=com.docker.compose.service=')) {
      return container.service === expected.slice('com.docker.compose.service='.length);
    }
    if (filter === 'label=com.docker.compose.oneoff=False') {
      return !container.oneoff;
    }
    return true;
  });
}

function inspectContainer(id, format) {
  const container = state.containers.find((item) => item.id === id);
  if (!container) fail(`unknown container ${id}`);
  if (format.includes('.State.Running')) return String(container.running);
  if (format.includes('.HostConfig.RestartPolicy.Name')) return container.restart ?? 'unless-stopped';
  if (format.includes('com.docker.compose.project')) return container.project;
  if (format.includes('com.docker.compose.service')) return container.service;
  if (format.includes('json .Config.Env')) return JSON.stringify([`RSCTF_ROLE=${container.role}`]);
  if (format.includes('.Config.Image')) return container.image;
  fail(`unsupported inspect format ${format}`);
}

function createContainers(service, count) {
  const config = state.configServices.find((item) => item.name === service);
  if (!config) fail(`unknown Compose service ${service}`);
  state.containers = state.containers.filter((item) => item.service !== service || item.oneoff);
  const base = state.nextContainerId ?? 1000;
  for (let index = 0; index < count; index += 1) {
    state.containers.push({
      id: (base + index).toString(16).padStart(12, '0'),
      project: state.project,
      service,
      oneoff: false,
      role: config.role,
      running: true,
      restart: 'unless-stopped',
      image: config.image,
    });
  }
  state.nextContainerId = base + count;
}

function runDockerCompose(values) {
  const commandIndex = values.findIndex((value) => ['config', 'stop', 'run', 'up'].includes(value));
  if (commandIndex < 0) fail(`unsupported docker compose invocation: ${values.join(' ')}`, 96);
  const action = values[commandIndex];
  const actionArgs = values.slice(commandIndex + 1);
  if (action === 'config') {
    output({
      services: Object.fromEntries([
        ...state.configServices.map((service) => [service.name, {
          image: service.image,
          environment: { RSCTF_ROLE: service.role },
        }]),
        ['db', { image: 'postgres:18', environment: {} }],
      ]),
    });
    return;
  }
  if (action === 'stop') {
    for (const container of state.containers) {
      if (actionArgs.includes(container.service) && !container.oneoff) container.running = false;
    }
    event('compose:stop');
    return;
  }
  if (action === 'run') {
    if (state.containers.some((item) => runtimeRoles.has(item.role) && item.running)) {
      fail('Compose migration invoked with a running runtime', 97);
    }
    event('compose:migrate');
    if (state.migrationFail) process.exit(1);
    state.migrationSucceeded = true;
    if (state.reappearAfterMigration) {
      const candidate = state.containers.find((item) => runtimeRoles.has(item.role));
      if (candidate) candidate.running = true;
    }
    save();
    return;
  }
  if (action === 'up') {
    if (!state.migrationSucceeded) fail('Compose runtime started before successful migration', 98);
    const scales = [];
    for (let index = 0; index < actionArgs.length; index += 1) {
      if (actionArgs[index] === '--scale') scales.push(actionArgs[index + 1]);
    }
    for (const value of scales) {
      const [service, rawCount] = value.split('=', 2);
      createContainers(service, Number(rawCount));
    }
    event(`compose:up:${scales.sort().join(',')}`);
    return;
  }
}

function runDocker() {
  if (args[0] === 'compose') {
    runDockerCompose(args.slice(1));
    return;
  }
  if (args[0] !== 'container') fail(`unsupported docker invocation: ${args.join(' ')}`, 99);
  const action = args[1];
  if (action === 'ls') {
    output(state.containers.filter((item) => containerMatches(item, args.slice(2))).map((item) => item.id).join('\n'));
    if (state.containers.some((item) => containerMatches(item, args.slice(2)))) output('\n');
    return;
  }
  if (action === 'inspect') {
    const format = optionValue(args, '--format');
    const id = args.at(-1);
    output(`${inspectContainer(id, format)}\n`);
    return;
  }
  if (action === 'stop') {
    const id = args.at(-1);
    const container = state.containers.find((item) => item.id === id);
    if (!container) fail(`unknown container ${id}`);
    container.running = false;
    event(`docker:stop:${id}`);
    output(`${id}\n`);
    return;
  }
  if (action === 'rm') {
    const id = args.at(-1);
    const container = state.containers.find((item) => item.id === id);
    if (!container || container.running) fail(`unsafe container removal ${id}`);
    state.containers = state.containers.filter((item) => item.id !== id);
    event(`docker:rm:${id}`);
    output(`${id}\n`);
    return;
  }
  fail(`unsupported docker container invocation: ${args.join(' ')}`, 99);
}

if (command === 'kubectl') runKubectl();
else if (command === 'helm') runHelm();
else if (command === 'docker') runDocker();
else fail(`unsupported fake command ${command}`, 89);
