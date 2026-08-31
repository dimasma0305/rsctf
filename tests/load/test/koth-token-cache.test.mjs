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
const managedKothLoad = readFileSync(new URL('../managed-koth.mjs', import.meta.url), 'utf8');

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
  assert.ok((hill.match(/current_capability_epochs/g) || []).length >= 2);
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

test('plaintext bearers stay local while cross-replica epochs are authoritative', () => {
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

test('epoch eviction recovery is writer-fenced and player rotation stays participant-scoped', () => {
  const current = section(
    capabilityCache,
    'pub(crate) async fn current_capability_epochs',
    'pub(crate) async fn begin_game_epoch_mutation',
  );
  assert.match(current, /PgAdvisoryLock::try_acquire_shared/);
  assert.ok(current.indexOf('try_acquire_shared') < current.indexOf('read_or_seed_epoch'));
  assert.match(
    capabilityCache,
    /evicted_mutation_markers_cannot_reseed_until_the_writer_commits/,
  );

  const rotation = section(
    tokens,
    'clear_unsettled_scores_for_capability_change',
    'Ok(no_store_token_response',
  );
  assert.match(rotation, /begin_participant_epoch_mutation/);
  assert.match(rotation, /finish_participant_epoch_mutation/);
  assert.match(rotation, /game_cache_mutation = if reconciled\.is_empty\(\)/);
  assert.match(rotation, /clear_unsettled_scores_for_capability_change[\s\S]*&\[part\.id\]/);
  assert.match(capabilityCache, /player_rotation_changes_only_that_participants_namespace/);
});

test('active-scoring emergency rotation is revisioned, retryable, and team-local', () => {
  const rotation = section(tokens, 'pub async fn rotate_koth_api_token', '#[cfg(test)]');
  assert.doesNotMatch(rotation, /scoring_started|ad_scoring_paused|load_manual_api_rotation_gate/);
  assert.match(rotation, /CredentialMutationInput\(request\)/);
  assert.match(rotation, /credential_operations::reserve/);
  assert.match(rotation, /CredentialReservation::Recovered/);
  assert.match(rotation, /credential_operations::complete/);
  assert.match(rotation, /rotate_player_api_capability/);
  assert.match(rotation, /token_rotation_cooldown_response/);
  assert.match(rotation, /clear_unsettled_scores_for_capability_change[\s\S]*&\[part\.id\]/);
  assert.doesNotMatch(rotation, /clear_unsettled_scores_for_capability_change[\s\S]*roster_snapshot/);
  assert.match(managedKothLoad, /exerciseActivePlayerRotation/);
  assert.match(managedKothLoad, /NOT ad_scoring_paused/);
  assert.match(managedKothLoad, /recovered\?\.token === rotated\.token/);
  assert.match(managedKothLoad, /JSON\.stringify\(otherAfter\) === JSON\.stringify\(otherBefore\)/);
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

  const gameControlRelease = capabilityCache.slice(
    capabilityCache.indexOf('pub(crate) async fn release_game_control'),
  );
  const releaseMarker = gameControlRelease.indexOf('begin_game_epoch_mutation_if');
  const releaseCommit = gameControlRelease.indexOf('control\n        .release', releaseMarker);
  const releasePublish = gameControlRelease.indexOf(
    'finish_game_epoch_mutation_if_any',
    releaseCommit,
  );
  assert.ok(
    releaseMarker >= 0 && releaseCommit > releaseMarker && releasePublish > releaseCommit,
  );
});

test('deterministic no-write edit exits restore the cache epoch', () => {
  const approval = section(challengeReview, 'pub async fn approve_challenge', 'pub async fn reject_challenge');
  assertRestorePrecedes(approval, 'Err(error) =>', 'return Err(error)');
  assertRestorePrecedes(approval, 'if updated != 1', 'Challenge review state changed');
  assertRestorePrecedes(challengeReview, 'if rejected != 1', 'Challenge is being deleted');
  assertRestorePrecedes(adEdit, 'if toggled != 1', 'Challenge is being deleted');
  const topologyBegin = challengeEdit.indexOf('topology_transition::begin(');
  const topologyCommit = challengeEdit.indexOf('release_game_control(', topologyBegin);
  assert.ok(topologyBegin >= 0 && topologyCommit > topologyBegin);

  const deletion = section(challengeEdit, 'pub async fn delete_challenge', 'pub(crate) struct BuildOutcome');
  const fenceFailure = deletion.indexOf('if let Err(error)');
  const restore = deletion.indexOf('finish_game_epoch_mutation_if_any', fenceFailure);
  const rejected = deletion.indexOf('return Err(error)', restore);
  assert.ok(fenceFailure >= 0 && restore > fenceFailure && rejected > restore);

  const approvalCommit = approval.indexOf('lock.release()', approval.indexOf('if updated != 1'));
  const approvalPublish = approval.indexOf('finish_game_epoch_mutation_if_any', approvalCommit);
  assert.ok(approvalCommit >= 0 && approvalPublish > approvalCommit);
  const deletionCommit = deletion.indexOf('engine_control\n        .release()');
  const deletionPublish = deletion.indexOf('finish_game_epoch_mutation_if_any', deletionCommit);
  assert.ok(deletionCommit >= 0 && deletionPublish > deletionCommit);
});
