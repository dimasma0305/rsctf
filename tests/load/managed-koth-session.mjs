import { randomUUID } from 'node:crypto';

/** Establish the same live-event admission as a real participant login. */
export async function loginManagedPlayer(
  { gameId, userId, teamId, participationId },
  { api, adminJwt, sql, mintJwt },
) {
  const reset = await api(
    'DELETE',
    `/api/admin/users/${encodeURIComponent(userId)}/password?operationId=${randomUUID()}`,
    { jwt: adminJwt(), timeoutMs: 10_000 },
  );
  const password = reset.json && Object.hasOwn(reset.json, 'data') ? reset.json.data : reset.json;
  if (reset.status !== 200 || typeof password !== 'string' || password.length < 8) {
    throw new Error(`managed player's password reset returned ${reset.status}`);
  }
  const userName = sql(`SELECT user_name FROM "AspNetUsers" WHERE id='${userId}'::uuid`);
  const login = await api('POST', '/api/account/login', {
    body: { userName, password },
    timeoutMs: 10_000,
  });
  if (login.status !== 200) {
    throw new Error(`managed player's scoped login returned ${login.status}`);
  }
  const observations = Number(sql(
    `SELECT count(*) FROM "IdentityObservations" ` +
      `WHERE user_id='${userId}'::uuid AND game_id=${gameId} ` +
      `AND team_id=${teamId} AND participation_id=${participationId} ` +
      `AND observed_at_utc>=(` +
        `SELECT start_time_utc FROM "Games" WHERE id=${gameId}) ` +
      `AND observed_at_utc<(` +
        `SELECT end_time_utc FROM "Games" WHERE id=${gameId}) ` +
      `AND observed_at_utc<=clock_timestamp()`,
  ));
  if (!Number.isSafeInteger(observations) || observations <= 0) {
    throw new Error('managed player login did not retain a live-event identity observation');
  }
  return mintJwt(userId, undefined, 1);
}
