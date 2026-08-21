import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import {
  parseDockerStat,
  parseFilesystemStat,
  parseImageStorageContext,
  parseProcessStat,
  summarizeResourceSamples,
} from '../image-storage.js';

const k6 = readFileSync(new URL('../k6/image-storage.js', import.meta.url), 'utf8');
const runner = readFileSync(new URL('../image-storage.mjs', import.meta.url), 'utf8');

test('image storage context requires distinct player JWTs', () => {
  const context = parseImageStorageContext(JSON.stringify({
    gameId: 1,
    challengeId: 2,
    tokens: ['a.b.c', 'd.e.f'],
  }));
  assert.deepEqual([...context.tokens], ['a.b.c', 'd.e.f']);
  assert.throws(
    () => parseImageStorageContext(JSON.stringify({ gameId: 1, challengeId: 2, tokens: ['a.b.c', 'a.b.c'] })),
    /unique JWTs/,
  );
});

test('resource samples preserve exact health and maxima', () => {
  assert.deepEqual(parseDockerStat('rsctf|12.5%|1 MiB'), {
    name: 'rsctf',
    cpuPercent: 12.5,
    memoryBytes: 1048576,
  });
  assert.deepEqual(parseFilesystemStat(' 1B-blocks Avail\n1000 250\n'), {
    totalBytes: 1000,
    availableBytes: 250,
  });
  assert.deepEqual(parseProcessStat('12.5 2048', 123), {
    name: 'pid:123',
    cpuPercent: 12.5,
    memoryBytes: 2097152,
  });
  assert.deepEqual(summarizeResourceSamples([
    {
      healthStatus: 200,
      healthBody: 'ok',
      filesystem: { availableBytes: 300 },
      resources: [parseDockerStat('rsctf|10%|100 B')],
    },
    {
      healthStatus: 503,
      healthBody: 'no',
      filesystem: { availableBytes: 250 },
      resources: [parseDockerStat('rsctf|20%|200 B')],
    },
  ]), {
    samples: 2,
    maxCpuPercent: 20,
    maxMemoryBytes: 200,
    minimumFilesystemAvailableBytes: 250,
    healthFailures: 1,
  });
});

test('k6 uses a fixed arrival burst and fails on health or start loss', () => {
  assert.match(k6, /executor: 'constant-arrival-rate'/);
  assert.match(k6, /iteration >= CONTEXT\.tokens\.length/);
  assert.match(k6, /image_start_attempts: \[`count==\$\{CONTEXT\.tokens\.length\}`\]/);
  assert.match(k6, /server_5xx: \['rate==0'\]/);
  assert.match(k6, /health_failure: \['rate==0'\]/);
  assert.match(k6, /dropped_iterations: \['count==0'\]/);
});

test('runner refuses ambient targets and proves exactly one RuntimeStart build', () => {
  assert.match(runner, /IMAGE_STORAGE_STRESS_ACK === '1'/);
  assert.match(runner, /ALLOW_REMOTE_IMAGE_STORAGE_STRESS/);
  assert.match(runner, /trigger='RuntimeStart'/);
  assert.match(runner, /audit\.runtimeBuilds !== beforeBuilds \+ 1/);
  assert.match(runner, /audit\.containers !== players\.length/);
  assert.doesNotMatch(runner, /UPDATE "GameChallenges"|DELETE FROM "BuildImageOwnerships"/);
});
