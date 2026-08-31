import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const read = (path) => readFileSync(new URL(path, import.meta.url), 'utf8');
const rateLimiterCore = read('../../../src/middlewares/rate_limiter.rs');
const kothAdmission = read('../../../src/middlewares/rate_limiter/koth.rs');
const rateLimiter = `${rateLimiterCore}\n${kothAdmission}`;
const authentication = read(
  '../../../src/controllers/game/koth/api/authentication.rs',
);
const kothRoutes = read('../../../src/controllers/game/koth/mod.rs');
const capabilityService = read('../../../src/services/ad/koth_api_capability.rs');
const kothApiEngine = read('../../../src/services/ad/engine/koth_api.rs');
const localCompose = read('../../../docker-compose.yml');
const deployCompose = read('../../../deploy/compose.yml');
const localEnvironment = read('../../../.env.example');
const deployEnvironment = read('../../../deploy/.env.example');
const configurationReference = read('../../../docs/reference/configuration.md');
const chartValues = read('../../../charts/rsctf/values.yaml');
const chartConfigMap = read('../../../charts/rsctf/templates/configmap.yaml');
const chartSchema = JSON.parse(read('../../../charts/rsctf/values.schema.json'));
const chartValidation = read('../../../scripts/validate-helm-chart.sh');

const rosterLimit = Number(
  kothApiEngine
    .match(/MAX_LEADERBOARD_TEAMS:\s*usize\s*=\s*([\d_]+)/)?.[1]
    .replaceAll('_', ''),
);
const sourceLimit = Number(
  rateLimiter
    .match(/DEFAULT_SOURCE_ADMISSION_PER_MINUTE:\s*u32\s*=\s*([\d_]+)/)?.[1]
    .replaceAll('_', ''),
);

test('managed KotH admits the complete 2,000-team same-source login wave', () => {
  assert.equal(rosterLimit, 2_000);
  assert.ok(
    sourceLimit >= rosterLimit,
    `${sourceLimit} source admissions cannot serve ${rosterLimit} teams`,
  );
  assert.ok(
    Number.isSafeInteger(sourceLimit) && sourceLimit <= 100_000,
    'source abuse ceiling is not finite and bounded',
  );
  const legitimateRatePerSecond = 100;
  const lifecycleSeconds = 20;
  assert.equal(legitimateRatePerSecond * lifecycleSeconds, rosterLimit);
  const refillPerSecond = sourceLimit / 60;
  assert.ok(refillPerSecond >= legitimateRatePerSecond);
  assert.equal(
    Math.min(
      sourceLimit,
      sourceLimit + refillPerSecond * lifecycleSeconds - rosterLimit,
    ),
    sourceLimit,
    'the maintained fixed-rate profile unexpectedly drains the default source bucket',
  );

  assert.match(
    rateLimiter,
    /AUTH_PATH:\s*&str\s*=\s*"\/api\/v1\/koth\/capability\/authenticate"/,
  );
  assert.match(rateLimiter, /method == Method::POST && path == AUTH_PATH/);
  assert.match(
    rateLimiterCore,
    /Policy::KothCapabilityAdmission\s*=>\s*koth::source_admission_kind\(\)/,
  );
  assert.match(
    kothAdmission,
    /pub\(super\) fn source_admission_kind[\s\S]*?Kind::Bucket/,
  );

  const middleware = rateLimiterCore.slice(
    rateLimiterCore.indexOf('pub async fn global_middleware'),
  );
  const dedicatedAdmission = middleware.indexOf('koth::admit_source(ip)');
  const genericCredentialLookup = middleware.indexOf(
    'privilege_authentication::session_token(req.headers())',
  );
  assert.ok(
    dedicatedAdmission >= 0 && dedicatedAdmission < genericCredentialLookup,
  );
});

test('the isolated abuse ceiling is configurable without allowing undersized rosters', () => {
  assert.match(
    kothAdmission,
    /MIN_SOURCE_ADMISSION_PER_MINUTE:\s*u32\s*=\s*3_000/,
  );
  assert.match(
    kothAdmission,
    /MAX_SOURCE_ADMISSION_PER_MINUTE:\s*u32\s*=\s*1_000_000/,
  );
  assert.match(
    rateLimiter,
    /std::env::var\("RSCTF_KOTH_CAPABILITY_IP_ADMISSION_PER_MINUTE"\)/,
  );
  for (const compose of [localCompose, deployCompose]) {
    assert.match(
      compose,
      /RSCTF_KOTH_CAPABILITY_IP_ADMISSION_PER_MINUTE:[^\n]*:-6000/,
    );
  }
  for (const documentation of [
    localEnvironment,
    deployEnvironment,
    configurationReference,
  ]) {
    assert.match(documentation, /RSCTF_KOTH_CAPABILITY_IP_ADMISSION_PER_MINUTE/);
  }

  const isolatedLimit = 3_000;
  const isolatedRefill = isolatedLimit / 60;
  const afterLegitimateWave = isolatedLimit + (isolatedRefill - 100) * 20;
  const secondsUntilAbuseDenial = afterLegitimateWave / (200 - isolatedRefill);
  assert.ok(secondsUntilAbuseDenial > 0 && secondsUntilAbuseDenial < 30);
});

test('Helm forwards and bounds managed KotH capability admission', () => {
  assert.match(chartValues, /kothCapabilityIpAdmissionPerMinute:\s*6000/);
  assert.match(
    chartConfigMap,
    /RSCTF_KOTH_CAPABILITY_IP_ADMISSION_PER_MINUTE: \{\{ \.Values\.config\.kothCapabilityIpAdmissionPerMinute \| quote \}\}/,
  );
  assert.ok(
    chartSchema.properties.config.required.includes(
      'kothCapabilityIpAdmissionPerMinute',
    ),
  );
  assert.deepEqual(
    chartSchema.properties.config.properties.kothCapabilityIpAdmissionPerMinute,
    {
      type: 'integer',
      minimum: 3000,
      maximum: 1000000,
      description:
        'Shared-arena source admission before managed Leaderboard KotH capability lookup.',
    },
  );
  assert.match(chartValidation, /config\.kothCapabilityIpAdmissionPerMinute=3000/);
  assert.match(chartValidation, /for invalid_admission in 2999 1000001/);
});

test('capability shape and source abuse are bounded before database authentication', () => {
  const shapeCheck = authentication.indexOf('validated_token(&request.token)');
  const concurrencyCheck = authentication.indexOf(
    'try_database_lookup_slot()',
    shapeCheck,
  );
  const poolAcquire = authentication.indexOf('.pg()');
  assert.ok(shapeCheck >= 0 && shapeCheck < poolAcquire);
  assert.ok(concurrencyCheck > shapeCheck && concurrencyCheck < poolAcquire);
  assert.match(authentication, /is_well_formed\(token\)/);
  assert.match(authentication, /DATABASE_LOOKUP_CONCURRENCY:\s*usize\s*=\s*8/);
  assert.match(authentication, /too_many_requests\(1\)/);
  assert.match(
    kothRoutes,
    /post\(authenticate_capability\)\.layer\(DefaultBodyLimit::max\(1_024\)\)/,
  );

  assert.match(
    kothAdmission,
    /check_async\(Policy::KothCapabilityAdmission, ip\)[\s\S]*?map\(too_many_requests\)/,
  );
});

test('authenticated fairness uses canonical hill participation without consuming reporter quota', () => {
  assert.match(
    capabilityService,
    /SELECT game_id, challenge_id, participation_id, team_name FROM eligible/,
  );
  assert.match(
    authentication,
    /admit_koth_capability_auth\(\s*identity\.game_id,\s*identity\.challenge_id,\s*identity\.participation_id,/,
  );
  const poolRelease = authentication.indexOf('drop(connection)');
  const identityAdmission = authentication.indexOf('admit_koth_capability_auth');
  assert.ok(
    poolRelease >= 0 && poolRelease < identityAdmission,
    'rate-limit I/O retains PostgreSQL',
  );
  assert.match(
    kothAdmission,
    /koth-capability:game:\{game_id\}:challenge:\{challenge_id\}:participation:\{participation_id\}/,
  );
  assert.match(
    kothAdmission,
    /pub\(crate\) async fn admit_authenticated[\s\S]*?check_async\(Policy::Global, key\)/,
  );

  // Reporter routes must continue through their own anonymous Global partition;
  // only the exact player capability exchange receives dedicated admission.
  assert.doesNotMatch(
    kothAdmission.match(/AUTH_PATH:[^\n]+/)?.[0] ?? '',
    /context|observations/,
  );
});
