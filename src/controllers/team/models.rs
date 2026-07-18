//! Request and response models for the team API.

use uuid::Uuid;

/// Body for create/update — `TeamUpdateModel`.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamUpdateModel {
    pub name: Option<String>,
    pub bio: Option<String>,
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
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamUserInfoModel {
    pub id: Uuid,
    pub user_name: Option<String>,
    pub bio: Option<String>,
    pub avatar: Option<String>,
    pub captain: bool,
    // RSCTF marks these `[JsonIgnore]`: populated for scoreboard generation but
    // never emitted to clients (they are PII). `GET /api/team/{id}` is public.
    #[serde(skip_serializing)]
    pub real_name: String,
    #[serde(skip_serializing)]
    pub student_number: String,
}

/// Team view — `TeamInfoModel`.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamInfoModel {
    pub id: i32,
    pub name: String,
    pub bio: Option<String>,
    pub avatar: Option<String>,
    pub locked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub members: Option<Vec<TeamUserInfoModel>>,
}
