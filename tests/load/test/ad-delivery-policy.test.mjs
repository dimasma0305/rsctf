import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const baseCompose = readFileSync(
  new URL('../../../deploy/compose.yml', import.meta.url),
  'utf8',
);
const roleCompose = readFileSync(
  new URL('../../../deploy/compose.roles.yml', import.meta.url),
  'utf8',
);
const localCompose = readFileSync(
  new URL('../../../docker-compose.yml', import.meta.url),
  'utf8',
);

const forwardedPolicy = [
  'RSCTF_AD_FLAG_PUSH_CONCURRENCY: ${RSCTF_AD_FLAG_PUSH_CONCURRENCY:-64}',
  'RSCTF_AD_FLAG_PUSH_ATTEMPTS: ${RSCTF_AD_FLAG_PUSH_ATTEMPTS:-3}',
  'RSCTF_AD_FLAG_PUSH_TIMEOUT_SECONDS: ${RSCTF_AD_FLAG_PUSH_TIMEOUT_SECONDS:-2}',
];

test('all-in-one and split control roles receive the same flag-delivery policy', () => {
  for (const setting of forwardedPolicy) {
    assert.equal(baseCompose.split(setting).length - 1, 1, setting);
    assert.equal(roleCompose.split(setting).length - 1, 1, setting);
    const localSetting = setting.replace(': ', ': "').concat('"');
    assert.equal(localCompose.split(localSetting).length - 1, 1, localSetting);
  }
});
