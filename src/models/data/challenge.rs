//! Challenges (flattening RSCTF `Challenge` base + `GameChallenge`),
//! flags, attachments, blob files, and challenge reviews.

pub mod game_challenge {
    use crate::utils::enums::{
        ChallengeBuildStatus, ChallengeCategory, ChallengeReviewStatus, ChallengeType, NetworkMode,
        ScoreCurve,
    };
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "GameChallenges")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub game_id: i32,

        // --- Challenge base ---
        pub title: String,
        pub content: String,
        pub category: ChallengeCategory,
        #[sea_orm(column_name = "Type")]
        #[serde(rename = "type")]
        pub challenge_type: ChallengeType,
        pub hints: Option<Json>,
        pub is_enabled: bool,
        pub deadline_utc: Option<DateTime<Utc>>,
        pub submission_limit: i32,
        pub accepted_count: i32,
        pub submission_count: i32,
        pub container_image: Option<String>,
        pub memory_limit: Option<i32>,
        pub storage_limit: Option<i32>,
        pub cpu_count: Option<i32>,
        pub expose_port: Option<i32>,
        /// Optional worker-plane aggregate definition. Input is validated before
        /// persistence; JSONB keeps the protocol representation byte-compatible.
        pub workload_spec: Option<Json>,
        pub file_name: Option<String>,
        pub flag_template: Option<String>,
        pub review_status: ChallengeReviewStatus,
        pub review_note: Option<String>,
        pub submitted_by_user_id: Option<Uuid>,
        pub submitted_at_utc: Option<DateTime<Utc>>,
        pub reviewed_at_utc: Option<DateTime<Utc>>,
        pub original_archive_blob_path: Option<String>,
        /// Optional build-context selector inside the immutable source archive:
        /// `.` means archive root, while a canonical relative path (currently
        /// `src`) selects and strips that subtree. `None` is audit-only source.
        pub build_context_subdir: Option<String>,
        pub build_status: ChallengeBuildStatus,
        pub build_image_digest: Option<String>,
        pub last_build_log: Option<String>,
        pub source_yaml_path: Option<String>,
        pub attachment_id: Option<i32>,
        pub test_container_id: Option<Uuid>,

        // --- Jeopardy dynamic scoring ---
        pub enable_traffic_capture: bool,
        pub enable_shared_container: bool,
        pub disable_blood_bonus: bool,
        pub original_score: i32,
        pub min_score_rate: f64,
        pub difficulty: f64,
        pub score_curve: ScoreCurve,
        pub shared_container_id: Option<Uuid>,
        /// Container network mode (RSCTF `Challenge.NetworkMode`, nullable, default
        /// `Open`). String on the wire (`"Open"`/`"Isolated"`/`"Custom"`).
        pub network_mode: Option<NetworkMode>,

        // --- Attack-Defense ---
        pub ad_checker_image: Option<String>,
        pub ad_allow_egress: bool,
        pub ad_allow_self_reset: bool,
        pub ad_ssh_requires_flag: bool,
        pub ad_self_hosted: bool,
        /// Manual epoch scoring weight, normalized within the fixed epoch budget.
        pub ad_scoring_weight: f64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod flag_context {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "FlagContexts")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub flag: String,
        pub is_occupied: bool,
        pub attachment_id: Option<i32>,
        pub challenge_id: Option<i32>,
        pub exercise_id: Option<i32>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod attachment {
    use crate::utils::enums::FileType;
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "Attachments")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        #[sea_orm(column_name = "Type")]
        #[serde(rename = "type")]
        pub file_type: FileType,
        pub remote_url: Option<String>,
        pub local_file_id: Option<i32>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod local_file {
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "Files")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub hash: String,
        pub upload_time_utc: DateTime<Utc>,
        pub file_size: i64,
        pub name: String,
        pub reference_count: i64,
    }

    impl Model {
        /// Sharded on-disk location: `{hash[..2]}/{hash[2..4]}`.
        pub fn location(&self) -> String {
            format!("{}/{}", &self.hash[..2], &self.hash[2..4])
        }
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod challenge_review {
    use crate::utils::enums::ReviewRating;
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "ChallengeReviews")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub challenge_id: i32,
        pub user_id: Uuid,
        pub game_id: i32,
        pub rating: ReviewRating,
        pub comment: Option<String>,
        pub submit_time_utc: DateTime<Utc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
