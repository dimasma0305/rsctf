use super::*;

#[derive(sqlx::FromRow)]
struct ExtensionCandidate {
    id: Uuid,
    status: i16,
    started_at: DateTime<Utc>,
    expect_stop_at: DateTime<Utc>,
    is_proxy: bool,
    ip: String,
    port: i32,
    public_ip: Option<String>,
    public_port: Option<i32>,
}

impl ExtensionCandidate {
    fn into_model(self) -> AppResult<ContainerInfoModel> {
        let status = match self.status {
            0 => ContainerStatus::Pending,
            1 => ContainerStatus::Running,
            2 => ContainerStatus::Destroyed,
            value => {
                return Err(AppError::internal(format!(
                    "container {} has invalid status {value}",
                    self.id
                )))
            }
        };
        let entry = if self.is_proxy {
            self.id.to_string()
        } else {
            let ip = self.public_ip.as_deref().unwrap_or(&self.ip);
            let port = self.public_port.unwrap_or(self.port);
            format!("{ip}:{port}")
        };
        Ok(ContainerInfoModel {
            id: self.id.to_string(),
            status,
            started_at: self.started_at,
            expect_stop_at: self.expect_stop_at,
            entry,
        })
    }
}

/// Extend only the runtime still owned by the refreshed player snapshot.
///
/// The caller must hold `game-container:{participation_id}`. Every writer of the
/// instance link uses that same PostgreSQL advisory identity, so this post-lock
/// read and immutable-ID comparison fence delayed A -> B request ordering.
pub(super) async fn extend_expected_team_container_locked(
    st: &SharedState,
    participation_id: i32,
    challenge_id: i32,
    expected_container_id: Uuid,
    policy: &crate::services::container_policy::ContainerPolicy,
) -> AppResult<ContainerInfoModel> {
    let current_container_id = sqlx::query_scalar::<_, Option<Uuid>>(
        r#"SELECT container_id
             FROM "GameInstances"
            WHERE participation_id = $1
              AND challenge_id = $2"#,
    )
    .bind(participation_id)
    .bind(challenge_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("No instance for this challenge"))?
    .ok_or_else(|| AppError::bad_request("No running container"))?;
    if current_container_id != expected_container_id {
        return Err(AppError::conflict(
            "The challenge instance changed; refresh and retry.",
        ));
    }

    let mut current = sqlx::query_as::<_, ExtensionCandidate>(
        r#"SELECT id, status, started_at, expect_stop_at, is_proxy, ip, port,
                  public_ip, public_port
             FROM "Containers"
            WHERE id = $1"#,
    )
    .bind(current_container_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Container not found"))?;

    // RSCTF permits renewal only once the runtime enters the configured window.
    if current.expect_stop_at - Utc::now()
        > chrono::Duration::minutes(i64::from(policy.renewal_window))
    {
        return Err(AppError::bad_request(
            "The container is not yet eligible for extension",
        ));
    }

    let stop_at =
        current.expect_stop_at + chrono::Duration::minutes(i64::from(policy.extension_duration));
    current.expect_stop_at = sqlx::query_scalar(
        r#"UPDATE "Containers"
              SET expect_stop_at = $2
            WHERE id = $1
        RETURNING expect_stop_at"#,
    )
    .bind(current_container_id)
    .bind(stop_at)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    current.into_model()
}
