//! Retained authorization fence for endpoints that publish live target addresses.

use super::*;
use axum::response::{IntoResponse, Response};

/// Request-local authentication state for endpoints that publish live target
/// addresses. The first lookup identifies the roster lock to take; this value
/// is always revalidated after that shared lock is retained.
pub(crate) struct LiveTargetCaller {
    pub(crate) participation: participation::Model,
    authentication: LiveTargetAuthentication,
}

enum LiveTargetAuthentication {
    Session {
        user_id: uuid::Uuid,
        security_stamp: String,
    },
    TeamToken(String),
}

impl LiveTargetCaller {
    /// Preserve dual-auth precedence while retaining the exact request
    /// credential needed for the authoritative fenced recheck.
    pub(crate) async fn resolve(
        st: &SharedState,
        headers: &HeaderMap,
        verified: Option<&crate::services::ad::api_token::VerifiedTeamToken>,
        rejected: Option<&crate::services::ad::api_token::RejectedTeamToken>,
        maybe_user: MaybeUser,
        game_id: i32,
    ) -> AppResult<Self> {
        let session_user_id = maybe_user.0.as_ref().map(|user| user.id);
        let session_security_stamp = maybe_user
            .0
            .as_ref()
            .map(|user| user.security_stamp.clone());
        let presented_team_token = crate::services::ad::api_token::bearer_token(headers)
            .filter(|token| crate::services::ad::api_token::is_well_formed(token))
            .map(str::to_owned);
        let token_auth_selected = verified.is_some() || presented_team_token.is_some();
        let participation =
            resolve_ad_attacker(st, headers, verified, rejected, maybe_user, game_id).await?;
        let authentication = if token_auth_selected {
            LiveTargetAuthentication::TeamToken(presented_team_token.ok_or(AppError::Unauthorized)?)
        } else {
            LiveTargetAuthentication::Session {
                user_id: session_user_id.ok_or(AppError::Unauthorized)?,
                security_stamp: session_security_stamp.ok_or(AppError::Unauthorized)?,
            }
        };
        Ok(Self {
            participation,
            authentication,
        })
    }

    /// Revalidate immediately after all live reads, serialize the prepared
    /// model while the shared roster/credential locks are retained, then release
    /// the sole database connection. Keeping this boundary here prevents either
    /// handler from checking out a second pool connection behind its own fence.
    pub(crate) async fn finish_response<T: Serialize>(
        &self,
        pool: &sqlx::PgPool,
        data: T,
    ) -> AppResult<Response> {
        let roster = self.acquire_fence(pool).await?;
        let response = RequestResponse::ok(data).into_response();
        roster.release().await?;
        Ok(response)
    }

    async fn acquire_fence(
        &self,
        pool: &sqlx::PgPool,
    ) -> AppResult<crate::utils::single_flight::PgAdvisoryLock> {
        let key = crate::services::live_roster::lock_key(self.participation.team_id);
        let Some(mut roster) =
            crate::utils::single_flight::PgAdvisoryLock::try_acquire_shared(pool, &key).await?
        else {
            return Err(AppError::unavailable(
                "Team credentials are changing; retry this request",
            ));
        };
        if !live_target_caller_is_authorized_on(
            roster.transaction_mut(),
            &self.authentication,
            &self.participation,
        )
        .await?
        {
            roster.release().await?;
            return Err(match self.authentication {
                LiveTargetAuthentication::Session { .. } => AppError::Forbidden,
                LiveTargetAuthentication::TeamToken(_) => AppError::Unauthorized,
            });
        }
        Ok(roster)
    }
}

/// Authoritative live-target admission on the transaction that owns the shared
/// roster fence. Token admission additionally locks the exact credential and
/// participation rows so revocation or eligibility changes cannot finish before
/// this response.
async fn live_target_caller_is_authorized_on(
    connection: &mut sqlx::PgConnection,
    authentication: &LiveTargetAuthentication,
    part: &participation::Model,
) -> AppResult<bool> {
    match authentication {
        LiveTargetAuthentication::Session {
            user_id,
            security_stamp,
        } => {
            crate::services::ad::roster::user_allows_shared_credentials_on(
                connection,
                *user_id,
                security_stamp,
                part.game_id,
                part.team_id,
                part.id,
            )
            .await
        }
        LiveTargetAuthentication::TeamToken(token) => {
            if !crate::services::ad::roster::lock_team_shared_credentials_on(
                connection,
                part.team_id,
            )
            .await?
            {
                return Ok(false);
            }
            let verified =
                crate::services::ad::api_token::authenticate_on(connection, token).await?;
            if !verified.is_some_and(|credential| {
                credential.participation.id == part.id
                    && credential.participation.game_id == part.game_id
                    && credential.participation.team_id == part.team_id
            }) {
                return Ok(false);
            }

            // `authenticate_on` is an exact point-in-time check. Retain a row
            // share lock as well so DELETE/rotation of this same bearer secret
            // linearizes after the protected address response.
            let token_hash = crate::services::ad::api_token::hash(token);
            let credential_id: Option<i32> = sqlx::query_scalar(
                r#"SELECT credential.id
                     FROM "AdTeamApiTokens" credential
                     JOIN "Participations" participation
                       ON participation.id = credential.participation_id
                    WHERE credential.participation_id = $1
                      AND credential.token_hash = $2
                      AND participation.game_id = $3
                      AND participation.team_id = $4
                      AND participation.status = $5
                    FOR SHARE OF credential, participation"#,
            )
            .bind(part.id)
            .bind(token_hash)
            .bind(part.game_id)
            .bind(part.team_id)
            .bind(ParticipationStatus::Accepted as i16)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            Ok(credential_id.is_some())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::time::Duration;

    use axum::http::StatusCode;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use uuid::Uuid;

    use super::*;
    use crate::utils::enums::{ParticipationStatus, Role};

    struct Fixture {
        admin_pool: sqlx::PgPool,
        pool: sqlx::PgPool,
        schema: String,
        captain_id: Uuid,
        member_id: Uuid,
        team_id: i32,
        game_id: i32,
        participation_id: i32,
        token: String,
    }

    impl Fixture {
        async fn new(max_connections: u32) -> Self {
            let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
                .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
            let admin_pool = PgPoolOptions::new()
                .max_connections(1)
                .connect(&database_url)
                .await
                .unwrap();
            let schema = format!("rsctf_target_auth_{}", Uuid::new_v4().simple());
            sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
                .execute(&admin_pool)
                .await
                .unwrap();
            let options = PgConnectOptions::from_str(&database_url)
                .unwrap()
                .options([("search_path", schema.as_str())]);
            let pool = PgPoolOptions::new()
                .max_connections(max_connections)
                .connect_with(options)
                .await
                .unwrap();
            sqlx::raw_sql(
                r#"
                CREATE TABLE "AspNetUsers" (
                  id UUID PRIMARY KEY,
                  role SMALLINT NOT NULL,
                  email_confirmed BOOLEAN NOT NULL,
                  security_stamp TEXT
                );
                CREATE TABLE "Teams" (
                  id INTEGER PRIMARY KEY,
                  captain_id UUID NOT NULL,
                  deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
                );
                CREATE TABLE "TeamMembers" (
                  team_id INTEGER NOT NULL,
                  user_id UUID NOT NULL,
                  PRIMARY KEY (team_id, user_id)
                );
                CREATE TABLE "Games" (
                  id INTEGER PRIMARY KEY,
                  start_time_utc TIMESTAMPTZ NOT NULL,
                  end_time_utc TIMESTAMPTZ NOT NULL,
                  deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
                );
                CREATE TABLE "Participations" (
                  id INTEGER PRIMARY KEY,
                  status SMALLINT NOT NULL,
                  token TEXT NOT NULL,
                  writeup_id INTEGER,
                  game_id INTEGER NOT NULL,
                  team_id INTEGER NOT NULL,
                  division_id INTEGER,
                  suspicion_score INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE "UserParticipations" (
                  user_id UUID NOT NULL,
                  game_id INTEGER NOT NULL,
                  team_id INTEGER NOT NULL,
                  participation_id INTEGER NOT NULL
                );
                CREATE TABLE "IdentityObservations" (
                  user_id UUID NOT NULL,
                  game_id INTEGER NOT NULL,
                  team_id INTEGER NOT NULL,
                  participation_id INTEGER NOT NULL,
                  observed_at_utc TIMESTAMPTZ NOT NULL
                );
                CREATE TABLE "AdTeamApiTokens" (
                  id SERIAL PRIMARY KEY,
                  participation_id INTEGER NOT NULL UNIQUE,
                  token_hash TEXT NOT NULL UNIQUE,
                  last_used_at_utc TIMESTAMPTZ
                );
                "#,
            )
            .execute(&pool)
            .await
            .unwrap();

            let captain_id = Uuid::new_v4();
            let member_id = Uuid::new_v4();
            let team_id = (Uuid::new_v4().as_u128() % 1_000_000_000) as i32 + 1;
            let game_id = team_id + 1;
            let participation_id = team_id + 2;
            for user_id in [captain_id, member_id] {
                sqlx::query(
                    r#"INSERT INTO "AspNetUsers"
                         (id, role, email_confirmed, security_stamp)
                       VALUES ($1, $2, TRUE, 'current-stamp')"#,
                )
                .bind(user_id)
                .bind(Role::User as i16)
                .execute(&pool)
                .await
                .unwrap();
            }
            sqlx::query(
                r#"INSERT INTO "Games" (id, start_time_utc, end_time_utc)
                   VALUES ($1, now() - interval '1 hour', now() + interval '1 hour')"#,
            )
            .bind(game_id)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(r#"INSERT INTO "Teams" (id, captain_id) VALUES ($1, $2)"#)
                .bind(team_id)
                .bind(captain_id)
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES ($1, $2)"#)
                .bind(team_id)
                .bind(member_id)
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                r#"INSERT INTO "Participations"
                     (id, status, token, game_id, team_id)
                   VALUES ($1, $2, 'participation-token', $3, $4)"#,
            )
            .bind(participation_id)
            .bind(ParticipationStatus::Accepted as i16)
            .bind(game_id)
            .bind(team_id)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                r#"INSERT INTO "UserParticipations"
                     (user_id, game_id, team_id, participation_id)
                   VALUES ($1, $2, $3, $4)"#,
            )
            .bind(member_id)
            .bind(game_id)
            .bind(team_id)
            .bind(participation_id)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                r#"INSERT INTO "IdentityObservations"
                     (user_id, game_id, team_id, participation_id, observed_at_utc)
                   VALUES ($1, $2, $3, $4, now())"#,
            )
            .bind(member_id)
            .bind(game_id)
            .bind(team_id)
            .bind(participation_id)
            .execute(&pool)
            .await
            .unwrap();
            let token = format!("ad_{}", "a".repeat(43));
            sqlx::query(
                r#"INSERT INTO "AdTeamApiTokens"
                     (participation_id, token_hash, last_used_at_utc)
                   VALUES ($1, $2, now())"#,
            )
            .bind(participation_id)
            .bind(crate::services::ad::api_token::hash(&token))
            .execute(&pool)
            .await
            .unwrap();

            Self {
                admin_pool,
                pool,
                schema,
                captain_id,
                member_id,
                team_id,
                game_id,
                participation_id,
                token,
            }
        }

        fn participation(&self) -> participation::Model {
            participation::Model {
                id: self.participation_id,
                status: ParticipationStatus::Accepted,
                token: "participation-token".to_string(),
                writeup_id: None,
                game_id: self.game_id,
                team_id: self.team_id,
                division_id: None,
                suspicion_score: 0,
                competitive_admitted_at_utc: None,
            }
        }

        fn session_caller(&self, security_stamp: &str) -> LiveTargetCaller {
            LiveTargetCaller {
                participation: self.participation(),
                authentication: LiveTargetAuthentication::Session {
                    user_id: self.member_id,
                    security_stamp: security_stamp.to_string(),
                },
            }
        }

        fn token_caller(&self) -> LiveTargetCaller {
            LiveTargetCaller {
                participation: self.participation(),
                authentication: LiveTargetAuthentication::TeamToken(self.token.clone()),
            }
        }

        async fn reset_token(&self) {
            sqlx::query(
                r#"INSERT INTO "AdTeamApiTokens"
                     (participation_id, token_hash, last_used_at_utc)
                   VALUES ($1, $2, now())
                   ON CONFLICT (participation_id) DO UPDATE SET
                     token_hash = EXCLUDED.token_hash,
                     last_used_at_utc = EXCLUDED.last_used_at_utc"#,
            )
            .bind(self.participation_id)
            .bind(crate::services::ad::api_token::hash(&self.token))
            .execute(&self.pool)
            .await
            .unwrap();
        }

        async fn cleanup(self) {
            self.pool.close().await;
            sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
                .execute(&self.admin_pool)
                .await
                .unwrap();
        }
    }

    async fn caller_is_live(fixture: &Fixture, caller: &LiveTargetCaller) -> bool {
        let mut connection = fixture.pool.acquire().await.unwrap();
        live_target_caller_is_authorized_on(
            &mut connection,
            &caller.authentication,
            &caller.participation,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn live_targets_reject_retained_members_stale_sessions_and_invalid_team_tokens() {
        let fixture = Fixture::new(3).await;
        let session = fixture.session_caller("current-stamp");
        assert!(caller_is_live(&fixture, &session).await);

        sqlx::query(r#"DELETE FROM "TeamMembers" WHERE team_id = $1 AND user_id = $2"#)
            .bind(fixture.team_id)
            .bind(fixture.member_id)
            .execute(&fixture.pool)
            .await
            .unwrap();
        let retained: i64 =
            sqlx::query_scalar(r#"SELECT count(*) FROM "UserParticipations" WHERE user_id = $1"#)
                .bind(fixture.member_id)
                .fetch_one(&fixture.pool)
                .await
                .unwrap();
        assert_eq!(retained, 1, "historical attribution must remain durable");
        assert!(!caller_is_live(&fixture, &session).await);

        sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES ($1, $2)"#)
            .bind(fixture.team_id)
            .bind(fixture.member_id)
            .execute(&fixture.pool)
            .await
            .unwrap();
        assert!(!caller_is_live(&fixture, &fixture.session_caller("stale-stamp")).await);

        let token = fixture.token_caller();
        assert!(caller_is_live(&fixture, &token).await);
        sqlx::query(r#"DELETE FROM "AdTeamApiTokens" WHERE participation_id = $1"#)
            .bind(fixture.participation_id)
            .execute(&fixture.pool)
            .await
            .unwrap();
        assert!(!caller_is_live(&fixture, &token).await);

        fixture.reset_token().await;
        sqlx::query(r#"UPDATE "AspNetUsers" SET role = $1 WHERE id = $2"#)
            .bind(Role::Banned as i16)
            .bind(fixture.captain_id)
            .execute(&fixture.pool)
            .await
            .unwrap();
        assert!(
            !caller_is_live(&fixture, &token).await,
            "a banned roster member invalidates the shared bearer credential"
        );
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn target_response_fence_linearizes_direct_token_revocation() {
        let fixture = Fixture::new(3).await;
        let caller = fixture.token_caller();
        let key = crate::services::live_roster::lock_key(fixture.team_id);
        let mut reader =
            crate::utils::single_flight::PgAdvisoryLock::try_acquire_shared(&fixture.pool, &key)
                .await
                .unwrap()
                .unwrap();
        assert!(live_target_caller_is_authorized_on(
            reader.transaction_mut(),
            &caller.authentication,
            &caller.participation,
        )
        .await
        .unwrap());

        let mut revocation = fixture.pool.begin().await.unwrap();
        sqlx::query("SET LOCAL lock_timeout = '150ms'")
            .execute(&mut *revocation)
            .await
            .unwrap();
        let blocked = sqlx::query(r#"DELETE FROM "AdTeamApiTokens" WHERE participation_id = $1"#)
            .bind(fixture.participation_id)
            .execute(&mut *revocation)
            .await
            .expect_err("credential deletion must block behind the response fence");
        assert!(matches!(
            &blocked,
            sqlx::Error::Database(error) if error.code().as_deref() == Some("55P03")
        ));
        revocation.rollback().await.unwrap();
        reader.release().await.unwrap();
        let deleted = sqlx::query(r#"DELETE FROM "AdTeamApiTokens" WHERE participation_id = $1"#)
            .bind(fixture.participation_id)
            .execute(&fixture.pool)
            .await
            .unwrap();
        assert_eq!(deleted.rows_affected(), 1);
        assert!(!caller_is_live(&fixture, &caller).await);
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn one_connection_pool_finishes_targets_and_hills_response_fences() {
        let fixture = Fixture::new(1).await;
        let targets = crate::controllers::game::ad::AdTargetsModel {
            current_round: 1,
            challenges: Vec::new(),
        };
        let target_response = tokio::time::timeout(
            Duration::from_secs(2),
            fixture
                .session_caller("current-stamp")
                .finish_response(&fixture.pool, targets),
        )
        .await
        .expect("Targets response must not wait for a second pool connection")
        .unwrap();
        assert_eq!(target_response.status(), StatusCode::OK);

        let hills = Vec::<crate::controllers::game::koth::KothHillListItem>::new();
        let hill_response = tokio::time::timeout(
            Duration::from_secs(2),
            fixture.token_caller().finish_response(&fixture.pool, hills),
        )
        .await
        .expect("Koth/Hills response must not wait for a second pool connection")
        .unwrap();
        assert_eq!(hill_response.status(), StatusCode::OK);
        fixture.cleanup().await;
    }
}
