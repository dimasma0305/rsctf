use super::*;

/// RSCTF `RepoBindingScanResultModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoBindingScanResultModel {
    pub games_created: i32,
    pub games_updated: i32,
    pub challenges_imported: i32,
    pub challenges_updated: i32,
    pub failures: i32,
    pub messages: Vec<String>,
}

pub(crate) struct RepoBindingScanExecution {
    pub result: RepoBindingScanResultModel,
    pub ran_at: DateTime<Utc>,
    pub commit_sha: Option<String>,
    pub message: String,
}

/// RSCTF `RepoBindingInfoModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoBindingInfoModel {
    pub id: i32,
    pub repo_url: String,
    pub r#ref: Option<String>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub created_at_utc: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub last_scan_utc: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub next_scan_utc: Option<DateTime<Utc>>,
    pub interval_seconds: i32,
    pub status: String,
    pub last_commit_sha: Option<String>,
    pub last_scan_message: Option<String>,
    pub has_git_hub_token: bool,
    pub token_status: String,
    pub current_activity: Option<String>,
    pub push_on_edit: bool,
    pub push_backlog: i64,
    pub push_last_error: Option<String>,
    pub games: Vec<Value>,
}

/// `RepoBindingCreateModel` — POST /api/admin/repobindings.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoBindingCreateModel {
    pub repo_url: String,
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub github_token: Option<String>,
    #[serde(default)]
    pub interval_seconds: Option<i32>,
    #[serde(default)]
    pub run_immediately: Option<bool>,
}

/// `RepoBindingUpdateModel` — PUT /api/admin/repobindings/{id} (patch semantics).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoBindingUpdateModel {
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub interval_seconds: Option<i32>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub github_token: Option<String>,
    #[serde(default)]
    pub push_on_edit: Option<bool>,
}

/// `RepoBindingScanHistoryModel` — one past scan of a binding.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoBindingScanHistoryModel {
    pub id: i32,
    #[serde(with = "crate::utils::datetime::millis")]
    pub ran_at_utc: DateTime<Utc>,
    pub commit_sha: Option<String>,
    pub games_created: i32,
    pub games_updated: i32,
    pub challenges_imported: i32,
    pub challenges_updated: i32,
    pub failures: i32,
    pub messages: Option<String>,
}
