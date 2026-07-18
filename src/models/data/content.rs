//! Posts, runtime config key/value store, and API tokens.

pub mod post {
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "Posts")]
    pub struct Model {
        /// Short hash id (`string` in RSCTF).
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: String,
        pub title: String,
        pub summary: String,
        pub content: String,
        pub is_pinned: bool,
        pub tags: Option<Json>,
        pub author_id: Option<Uuid>,
        pub update_time_utc: DateTime<Utc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod config {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "Configs")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub config_key: String,
        pub value: Option<String>,
        pub cache_keys: Option<Json>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod api_token {
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "ApiTokens")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub name: String,
        /// SHA-256 hash of the presented bearer secret.
        #[serde(skip_serializing)]
        pub token_hash: String,
        pub creator_id: Option<Uuid>,
        pub created_at: DateTime<Utc>,
        pub expires_at: Option<DateTime<Utc>>,
        pub last_used_at: Option<DateTime<Utc>>,
        pub is_revoked: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
