use super::*;

/// Co-organizer view of a user (RSCTF `UserInfoModel`). The manager-list route is
/// typed `ProfileUserInfoModel[]` on the client, so the camelCase field set
/// mirrors that shape (`userId`/`userName`/`stdNumber`/`hasManagedGames`, ...).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerInfoModel {
    pub user_id: Uuid,
    pub user_name: Option<String>,
    pub email: Option<String>,
    pub role: Role,
    pub bio: String,
    pub real_name: String,
    pub std_number: String,
    pub phone: Option<String>,
    pub avatar: Option<String>,
    pub has_managed_games: bool,
}

impl ManagerInfoModel {
    pub(super) fn from_user(u: &user::Model) -> Self {
        Self {
            user_id: u.id,
            user_name: u.user_name.clone(),
            email: u.email.clone(),
            role: u.role,
            bio: u.bio.clone(),
            real_name: u.real_name.clone(),
            std_number: u.std_number.clone(),
            phone: u.phone_number.clone(),
            avatar: u.avatar_url(),
            has_managed_games: true,
        }
    }
}
