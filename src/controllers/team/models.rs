//! Request and response models for the team API.

use uuid::Uuid;

/// Body for create/update — `TeamUpdateModel`.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamUpdateModel {
    pub name: Option<String>,
    pub bio: Option<String>,
    #[serde(default)]
    pub profile_revision: i64,
    #[serde(default, skip_serializing)]
    pub operation_id: Option<Uuid>,
}

/// Body for `PUT /{id}/transfer` — `TeamTransferModel`.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamTransferModel {
    pub new_captain_id: Uuid,
}

/// Body for `POST /verify` — `SignatureVerifyModel`.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureVerifyModel {
    #[serde(default)]
    pub team_token: String,
    #[serde(default)]
    pub public_key: String,
}

/// One roster entry — `TeamUserInfoModel`.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamUserInfoModel {
    pub id: Uuid,
    pub user_name: Option<String>,
    pub bio: Option<String>,
    pub avatar: Option<String>,
    pub captain: bool,
    // RSCTF marks these `[JsonIgnore]`: populated for scoreboard generation but
    // never emitted to clients (they are PII). `GET /api/team/{id}` is public.
    #[serde(default, skip_serializing)]
    pub real_name: String,
    #[serde(default, skip_serializing)]
    pub student_number: String,
}

/// Team view — `TeamInfoModel`.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamInfoModel {
    pub id: i32,
    pub name: String,
    pub bio: Option<String>,
    pub avatar: Option<String>,
    pub locked: bool,
    pub profile_revision: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub members: Option<Vec<TeamUserInfoModel>>,
}

/// Compact team identity used by selectors that never need roster profiles.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamSelectorModel {
    pub id: i32,
    pub name: String,
}
