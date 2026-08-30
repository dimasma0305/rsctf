import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const tokens = readFileSync(
  new URL('../../../src/controllers/game/koth/tokens.rs', import.meta.url),
  'utf8',
);
const capabilityCache = readFileSync(
  new URL('../../../src/services/ad/koth_capability_cache.rs', import.meta.url),
  'utf8',
);
const cache = readFileSync(new URL('../../../src/services/cache.rs', import.meta.url), 'utf8');
const revocation = readFileSync(
  new URL('../../../src/services/ad/engine/koth_auth.rs', import.meta.url),
  'utf8',
);
const lifecycle = readFileSync(
  new URL('../../../src/services/ad/engine/koth_cycle/lifecycle/capability.rs', import.meta.url),
  'utf8',
);
const challengeReview = readFileSync(
  new URL('../../../src/controllers/edit/challenges/review.rs', import.meta.url),
  'utf8',
);
const adEdit = readFileSync(
  new URL('../../../src/controllers/edit/ad/mod.rs', import.meta.url),
  'utf8',
);
const challengeEdit = readFileSync(
  new URL('../../../src/controllers/edit/challenges/mod.rs', import.meta.url),
  'utf8',
);
const panel = readFileSync(
  new URL('../../../web/src/components/KothChallengePanel.tsx', import.meta.url),
  'utf8',
);
const playerLoad = readFileSync(new URL('../k6/player.js', import.meta.url), 'utf8');

function section(source, start, end) {
  const from = source.indexOf(start);
  const to = source.indexOf(end, from + start.length);
  assert.ok(from >= 0 && to > from, `missing section ${start}`);
  return source.slice(from, to);
}

function assertRestorePrecedes(source, guard, rejection) {
  const guarded = source.indexOf(guard);
  const restore = source.indexOf('finish_game_epoch_mutation_if_any', guarded);
  const rejected = source.indexOf(rejection, guarded);
  assert.ok(guarded >= 0 && restore > guarded && rejected > restore, `missing restoration for ${guard}`);
}

test('the five-second managed KotH token poll has a bounded cache hit path', () => {
  assert.match(panel, /successDelay:\s*\(\) => jitterPollingDelay\(KOTH_POLL_INTERVAL_MS\)/);
  assert.match(playerLoad, /\/ad\/koth\/\$\{hill\}\/token/);

  const hill = section(tokens, 'pub async fn koth_hill_token', 'pub struct KothHillTokenModel');
  assert.match(hill, /load_latest_round_cached/);
  assert.ok((hill.match(/current_game_epoch/g) || []).length >= 2);
  assert.match(hill, /get_local\(&key\)/);
  assert.match(hill, /set_local\([\s\S]*TOKEN_MODEL_CACHE_TTL/);
  assert.doesNotMatch(hill, /if model\.token\.is_some\(\)/);
  assert.match(hill, /Warmup and reset-gap models are cached too/);
  assert.ok(hill.indexOf('get_local(&key)') < hill.indexOf('acquire_koth_token_read_fence'));
  assert.ok(hill.indexOf('load_latest_round_on') > hill.indexOf('cached_model'));

  const all = section(tokens, 'pub async fn koth_token_all', 'pub async fn rotate_koth_api_token');
  assert.match(all, /get_local\(&key\)/);
  assert.match(all, /set_local\([\s\S]*TOKEN_MODEL_CACHE_TTL/);
  assert.match(all, /latest_round == 0\s*\{\s*Vec::new\(\)/);
  assert.doesNotMatch(all, /if\s+!out\.is_empty\(\)/);

  assert.match(capabilityCache, /TOKEN_MODEL_CACHE_TTL:\s*Duration = Duration::from_secs\(10\)/);
  assert.match(cache, /const L1_MAX_ENTRIES:\s*usize = 4_096/);
});

test('plaintext bearers stay local while the cross-replica epoch is authoritative', () => {
  const tiered = section(cache, 'impl Cache for TieredCache', '#[cfg(test)]');
  assert.match(tiered, /get_local[\s\S]*self\.l1\.get/);
  assert.match(tiered, /set_local[\s\S]*self\.l1\.set/);
  assert.match(tiered, /get_authoritative[\s\S]*self\.l2\.get/);
  assert.match(tiered, /set_authoritative[\s\S]*self\.l2\.set/);
  assert.doesNotMatch(capabilityCache, /token:\s*Option<String>/);
  assert.match(capabilityCache, /set_if_absent_authoritative/);
  assert.match(capabilityCache, /finish_game_epoch_mutation/);
  assert.match(cache, /compare_and_set_authoritative[\s\S]*redis::Script/);
});

test('capability mutations disable before commit and ticket-finalize after it', () => {
  const direct = section(
    revocation,
    'pub(crate) async fn revoke_koth_capabilities(',
    'pub(crate) async fn reconcile_koth_capability_revocations(',
  );
  const mutation = direct.indexOf('revoke_game_capabilities');
  const preCommit = direct.indexOf('begin_game_epoch_mutation', mutation);
  const commit = direct.indexOf('lock.release', preCommit);
  const postCommit = direct.indexOf('invalidate_capability_cache', commit);
  assert.ok(mutation >= 0 && preCommit > mutation && commit > preCommit && postCommit > commit);

  const lifecycleBegin = lifecycle.indexOf('begin_game_epoch_mutation');
  const lifecycleCommit = lifecycle.indexOf('control\n        .release', lifecycleBegin);
  const lifecycleFinish = lifecycle.indexOf('finish_game_epoch_mutation', lifecycleCommit);
  assert.ok(lifecycleBegin >= 0 && lifecycleCommit > lifecycleBegin && lifecycleFinish > lifecycleCommit);
  assert.match(capabilityCache, /mutating:/);
  assert.match(capabilityCache, /decode_epoch[\s\S]*CAPABILITY_EPOCH_ENCODED_LEN/);
  assert.match(
    capabilityCache,
    /CAPABILITY_MUTATION_MARKER_TTL:\s*Option<Duration> = None/,
  );
  assert.match(capabilityCache, /compare_and_set_authoritative/);
  assert.match(capabilityCache, /two_phase_rotation_rejects_a_cross_replica_racing_fill/);
  assert.match(capabilityCache, /asymmetric_authoritative_failure_aborts_or_leaves_cache_disabled/);
  assert.match(capabilityCache, /stale_finalizer_cannot_overwrite_a_newer_replica_mutation_marker/);
  assert.match(capabilityCache, /newest_replica_mutation_can_publish_after_a_stale_finalizer/);
  assert.match(cache, /local_only_values_never_reach_the_shared_tier_or_another_replica/);
});

test('deterministic no-write edit exits restore the cache epoch', () => {
  const approval = section(challengeReview, 'pub async fn approve_challenge', 'pub async fn reject_challenge');
  assertRestorePrecedes(approval, 'Err(error) =>', 'return Err(error)');
  assertRestorePrecedes(approval, 'if updated != 1', 'Challenge review state changed');
  assertRestorePrecedes(challengeReview, 'if rejected != 1', 'Challenge is being deleted');
  assertRestorePrecedes(adEdit, 'if toggled != 1', 'Challenge is being deleted');
  assertRestorePrecedes(
    challengeEdit,
    'if fenced.rows_affected() != 1',
    'Challenge eligibility changed',
  );

  const deletion = section(challengeEdit, 'pub async fn delete_challenge', 'pub(crate) struct BuildOutcome');
  const fenceFailure = deletion.indexOf('if let Err(error)');
  const restore = deletion.indexOf('finish_game_epoch_mutation_if_any', fenceFailure);
  const rejected = deletion.indexOf('return Err(error)', restore);
  assert.ok(fenceFailure >= 0 && restore > fenceFailure && rejected > restore);

  const approvalCommit = approval.indexOf('lock.release()', approval.indexOf('if updated != 1'));
  const approvalPublish = approval.indexOf('finish_game_epoch_mutation_if_any', approvalCommit);
  assert.ok(approvalCommit >= 0 && approvalPublish > approvalCommit);
  const deletionCommit = deletion.indexOf('definition_lock.release().await?');
  const deletionPublish = deletion.indexOf('finish_game_epoch_mutation_if_any', deletionCommit);
  assert.ok(deletionCommit >= 0 && deletionPublish > deletionCommit);
});
