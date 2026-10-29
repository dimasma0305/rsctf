import assert from 'node:assert/strict';
import test from 'node:test';
import { loginManagedPlayer } from '../managed-koth-session.mjs';

const identity = {
  gameId: 7, userId: '55492fc7-f6b7-4e9a-8bd7-ebfe623e95e0', teamId: 11, participationId: 13,
};

function fixture({ resetStatus = 200, loginStatus = 200, observations = '1', envelope = false } = {}) {
  const calls = [];
  const password = 'fixture-password';
  const dependencies = {
    adminJwt: () => 'fixture-admin',
    async api(method, path, options) {
      if (method === 'DELETE') {
        assert.match(path, new RegExp(`^/api/admin/users/${identity.userId}/password\\?operationId=`));
        assert.equal(options.jwt, 'fixture-admin');
        calls.push('reset');
        return { status: resetStatus, json: envelope ? { data: password } : password };
      }
      assert.equal(method, 'POST');
      assert.equal(path, '/api/account/login');
      assert.deepEqual(options.body, { userName: 'fixture-user', password });
      calls.push('login');
      return { status: loginStatus, text: 'must not expose response credentials' };
    },
    sql(query) {
      assert.match(query, new RegExp(identity.userId));
      if (query.startsWith('SELECT user_name')) return 'fixture-user';
      assert.match(query, /game_id=7/);
      assert.match(query, /team_id=11 AND participation_id=13/);
      assert.match(query, /observed_at_utc>=/);
      assert.match(query, /observed_at_utc</);
      assert.match(query, /observed_at_utc<=clock_timestamp\(\)/);
      calls.push('observation');
      return observations;
    },
    mintJwt(userId, stamp, role) {
      assert.deepEqual([userId, stamp, role], [identity.userId, undefined, 1]);
      calls.push('session');
      return 'fixture-player';
    },
  };
  return { calls, dependencies };
}

for (const envelope of [false, true]) {
  test(`player admission logs in and verifies its live observation before minting (envelope=${envelope})`, async () => {
    const f = fixture({ envelope });
    assert.equal(await loginManagedPlayer(identity, f.dependencies), 'fixture-player');
    assert.deepEqual(f.calls, ['reset', 'login', 'observation', 'session']);
  });
}

test('missing or invalid admission evidence never yields a player session', async () => {
  for (const observations of ['0', '-1', 'NaN', '1.5']) {
    const f = fixture({ observations });
    await assert.rejects(loginManagedPlayer(identity, f.dependencies), /live-event identity observation/);
    assert.deepEqual(f.calls, ['reset', 'login', 'observation']);
  }
});

test('failed reset or login stops admission without exposing response contents', async () => {
  for (const [options, expected] of [
    [{ resetStatus: 403 }, ['reset']],
    [{ loginStatus: 401 }, ['reset', 'login']],
  ]) {
    const f = fixture(options);
    await assert.rejects(loginManagedPlayer(identity, f.dependencies), (error) => {
      assert.match(error.message, /returned (401|403)/);
      assert.doesNotMatch(error.message, /credentials|fixture-password/);
      return true;
    });
    assert.deepEqual(f.calls, expected);
  }
});
