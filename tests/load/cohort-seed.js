function positiveInteger(value, label) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(`${label} must be a positive integer (got ${value})`);
  }
  return parsed;
}

/**
 * Build one statement that creates a single-member accepted cohort atomically.
 * The final JSON result binds every generated database id to its input ordinal.
 */
export function cohortSeedQuery(gameId, count) {
  const gid = positiveInteger(gameId, "cohort game id");
  const size = positiveInteger(count, "cohort size");
  return `WITH cohort AS MATERIALIZED (
    SELECT ordinal,
           gen_random_uuid() AS user_id,
           'lt${gid}_' || ordinal AS user_name
      FROM generate_series(1,${size}) AS ordinal
), inserted_users AS (
    INSERT INTO "AspNetUsers"
      (id,user_name,normalized_user_name,email,normalized_email,email_confirmed,password_hash,
       security_stamp,concurrency_stamp,role,register_time_utc,last_signed_in_utc,last_visited_utc,
       lockout_enabled,access_failed_count,phone_number_confirmed,two_factor_enabled,
       ip,bio,real_name,std_number,exercise_visible)
    SELECT user_id,user_name,upper(user_name),
           user_name || '@load.test',upper(user_name || '@load.test'),
           true,'x-load-placeholder',gen_random_uuid()::text,gen_random_uuid()::text,
           1,now(),now(),now(),true,0,false,false,
           '0.0.0.0','','','',false
      FROM cohort
     ORDER BY ordinal
    RETURNING id
), inserted_teams AS (
    INSERT INTO "Teams"(name,invite_token,captain_id,locked,deletion_pending)
    SELECT 'LT${gid}_' || cohort.ordinal,
           substr(md5(gen_random_uuid()::text),1,32),
           inserted_users.id,
           false,
           false
      FROM cohort
      JOIN inserted_users ON inserted_users.id=cohort.user_id
     ORDER BY cohort.ordinal
    RETURNING id,captain_id
), inserted_members AS (
    INSERT INTO "TeamMembers"(team_id,user_id)
    SELECT id,captain_id FROM inserted_teams
    RETURNING team_id,user_id
), inserted_participations AS (
    INSERT INTO "Participations"(status,token,game_id,team_id,division_id,suspicion_score)
    SELECT 1,substr(md5(gen_random_uuid()::text),1,16),${gid},id,NULL,0
      FROM inserted_teams
    RETURNING id,team_id
), inserted_links AS (
    INSERT INTO "UserParticipations"(user_id,game_id,team_id,participation_id)
    SELECT member.user_id,${gid},participation.team_id,participation.id
      FROM inserted_participations participation
      JOIN inserted_members member ON member.team_id=participation.team_id
    RETURNING user_id,team_id,participation_id
)
SELECT COALESCE(json_agg(json_build_object(
         'ordinal',cohort.ordinal,
         'userId',link.user_id,
         'teamId',link.team_id,
         'partId',link.participation_id
       ) ORDER BY cohort.ordinal),'[]'::json)::text
  FROM cohort
  JOIN inserted_links link ON link.user_id=cohort.user_id`;
}

const uuidPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

/** Parse and validate the database-owned identity mapping returned by cohortSeedQuery. */
export function parseCohortSeedResult(output, expectedCount) {
  const expected = positiveInteger(expectedCount, "expected cohort size");
  let rows;
  try {
    rows = JSON.parse(String(output));
  } catch (error) {
    throw new Error(`cohort seed returned malformed JSON: ${error.message}`);
  }
  if (!Array.isArray(rows) || rows.length !== expected) {
    throw new Error(`cohort seed returned ${Array.isArray(rows) ? rows.length : "non-array"} rows; expected ${expected}`);
  }

  rows.sort((left, right) => Number(left?.ordinal) - Number(right?.ordinal));
  const userIds = [];
  const teamIds = [];
  const partIds = [];
  for (let index = 0; index < rows.length; index++) {
    const row = rows[index];
    const ordinal = Number(row?.ordinal);
    const teamId = Number(row?.teamId);
    const partId = Number(row?.partId);
    if (
      ordinal !== index + 1 ||
      !uuidPattern.test(String(row?.userId || "")) ||
      !Number.isSafeInteger(teamId) ||
      teamId <= 0 ||
      !Number.isSafeInteger(partId) ||
      partId <= 0
    ) {
      throw new Error(`cohort seed returned an invalid identity at ordinal ${index + 1}`);
    }
    userIds.push(row.userId);
    teamIds.push(teamId);
    partIds.push(partId);
  }
  if (new Set(userIds).size !== expected || new Set(teamIds).size !== expected || new Set(partIds).size !== expected) {
    throw new Error("cohort seed returned duplicate identities");
  }
  return { userIds, teamIds, partIds };
}
