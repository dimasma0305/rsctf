//! User account table. Mirrors RSCTF `UserInfo : IdentityUser<Guid>`,
//! flattening the ASP.NET Identity base columns into the model.

pub mod user {
    use crate::utils::enums::Role;
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "AspNetUsers")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,

        // --- ASP.NET Identity base columns ---
        #[sea_orm(unique)]
        pub user_name: Option<String>,
        #[sea_orm(unique)]
        pub normalized_user_name: Option<String>,
        pub email: Option<String>,
        pub normalized_email: Option<String>,
        pub email_confirmed: bool,
        #[serde(skip_serializing)]
        pub password_hash: Option<String>,
        #[serde(skip_serializing)]
        pub security_stamp: Option<String>,
        #[serde(skip_serializing)]
        pub concurrency_stamp: Option<String>,
        pub phone_number: Option<String>,
        pub phone_number_confirmed: bool,
        pub two_factor_enabled: bool,
        pub lockout_end: Option<DateTime<Utc>>,
        pub lockout_enabled: bool,
        pub access_failed_count: i32,

        // --- RSCTF custom columns ---
        pub role: Role,
        pub ip: String,
        pub browser_fingerprint: Option<String>,
        pub last_signed_in_utc: DateTime<Utc>,
        pub last_visited_utc: DateTime<Utc>,
        pub register_time_utc: DateTime<Utc>,
        pub bio: String,
        pub real_name: String,
        pub std_number: String,
        pub exercise_visible: bool,
        pub avatar_hash: Option<String>,
    }

    impl Model {
        pub fn avatar_url(&self) -> Option<String> {
            self.avatar_hash
                .as_ref()
                .map(|h| format!("/assets/{h}/avatar"))
        }
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
