//! Supplementary entities RSCTF has that the initial rsctf schema lacked:
//! team roster (many-to-many Team↔User), the audit log, and suspicion events.

pub mod team_member {
    //! A user's membership in a team (RSCTF `Team.Members`).
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "TeamMembers")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub team_id: i32,
        pub user_id: Uuid,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

pub mod log_entry {
    //! Structured audit log (RSCTF `LogModel` / `Logs` table).
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "Logs")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub time_utc: DateTime<Utc>,
        pub level: String,
        pub logger: String,
        pub remote_ip: Option<String>,
        pub user_name: Option<String>,
        #[sea_orm(column_type = "Text")]
        pub message: String,
        pub status: Option<String>,
        /// The submitting browser fingerprint captured at login (RSCTF
        /// `LogModel.BrowserFingerprint`); rendered as the admin Logs
        /// `fingerprint` column. `None` for events without a fingerprint.
        #[sea_orm(column_type = "Text", nullable)]
        pub browser_fingerprint: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

pub mod game_manager {
    //! A user granted organizer rights for one game (RSCTF `Game.Managers` /
    //! `EventManager`) — co-organizers who can edit the game without being a
    //! platform Admin.
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "GameManagers")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub game_id: i32,
        pub user_id: Uuid,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

pub mod suspicion_event {
    //! A recorded cheat-suspicion (RSCTF `CheatCheckInfo` / suspicion log).
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "SuspicionEvents")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub game_id: i32,
        pub participation_id: i32,
        pub challenge_id: Option<i32>,
        /// RSCTF suspicion rule code.
        pub kind: i16,
        /// Stable identity of the underlying incident (for example a submission).
        #[serde(skip)]
        pub evidence_key: String,
        /// Weight resolved when this immutable evidence row was first recorded.
        /// Legacy rows remain `None` and use the current configured fallback.
        #[serde(skip)]
        pub score_delta: Option<i32>,
        pub created_at: DateTime<Utc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}

    #[cfg(test)]
    mod tests {
        use super::Model;

        #[test]
        fn internal_evidence_fields_do_not_expand_the_model_json() {
            let model = Model {
                id: 1,
                game_id: 2,
                participation_id: 3,
                challenge_id: Some(4),
                kind: 0,
                evidence_key: "submission:5".to_string(),
                score_delta: Some(100),
                created_at: chrono::Utc::now(),
            };

            let json = serde_json::to_value(model).expect("serialize suspicion event");
            assert!(json.get("evidence_key").is_none());
            assert!(json.get("score_delta").is_none());
        }
    }
}

pub mod repo_binding {
    //! A watched git repository whose `challenge.yaml` manifests are imported as
    //! games/challenges (RSCTF `RepoBinding` / `RepoBindings` table).
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    use crate::utils::enums::RepoWatchStatus;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "RepoBindings")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub repo_url: String,
        pub git_ref: Option<String>,
        /// Stored access token; exposed only as `hasGitHubToken`/`tokenStatus`.
        pub github_token: Option<String>,
        pub interval_seconds: i32,
        pub status: RepoWatchStatus,
        pub last_commit_sha: Option<String>,
        pub last_scan_message: Option<String>,
        pub last_scan_utc: Option<DateTime<Utc>>,
        pub next_scan_utc: Option<DateTime<Utc>>,
        pub created_at_utc: DateTime<Utc>,
        pub push_on_edit: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

pub mod repo_binding_scan {
    //! One scan/import run of a [`super::repo_binding`] (its scan history).
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "RepoBindingScans")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub binding_id: i32,
        pub ran_at_utc: DateTime<Utc>,
        pub commit_sha: Option<String>,
        pub games_created: i32,
        pub games_updated: i32,
        pub challenges_imported: i32,
        pub challenges_updated: i32,
        pub failures: i32,
        pub messages: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

pub mod anti_cheat_block {
    //! A recorded anti-cheat conflict: a login blocked because its IP or browser
    //! fingerprint collided with another (teammate/global) account, per the admin
    //! RequireUnique* policy (RSCTF `AntiCheatBlock`).
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};
    use uuid::Uuid;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "AntiCheatBlocks")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub user_id: Uuid,
        pub user_name: Option<String>,
        pub conflict_user_id: Option<Uuid>,
        pub conflict_user_name: Option<String>,
        /// "Ip" | "Fingerprint" (AntiCheatBlockKind on the wire).
        pub kind: String,
        pub conflicting_value: Option<String>,
        pub occurred_at_utc: DateTime<Utc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

pub mod build_record {
    //! One challenge image build/pull attempt (RSCTF `ChallengeBuildRecord` /
    //! the admin Builds audit history).
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    use crate::utils::enums::ChallengeBuildStatus;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "BuildRecords")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub challenge_id: i32,
        pub game_id: i32,
        pub challenge_title: String,
        pub enqueued_at_utc: DateTime<Utc>,
        pub started_at_utc: Option<DateTime<Utc>>,
        pub finished_at_utc: Option<DateTime<Utc>>,
        /// BuildTrigger wire value: "Import"|"Manual"|"AutoRetry"|"Bulk".
        pub trigger: String,
        /// ChallengeBuildKind wire value: "Challenge"|"Checker".
        pub kind: String,
        pub attempt: i32,
        pub status: ChallengeBuildStatus,
        pub digest: Option<String>,
        pub image_ref: Option<String>,
        pub log_tail: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

pub mod ad_team_api_token {
    //! A per-team headless flag-submission token for Attack & Defense (RSCTF
    //! `AdTeamApiToken`). Only the hash is stored; the plaintext is shown once.
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "AdTeamApiTokens")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub participation_id: i32,
        pub token_hash: String,
        pub hint: String,
        pub created_at_utc: DateTime<Utc>,
        pub last_rotated_at_utc: Option<DateTime<Utc>>,
        pub last_used_at_utc: Option<DateTime<Utc>>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

pub mod ad_ssh_key {
    //! A per-team registered/generated SSH public key for Attack & Defense
    //! (RSCTF `AdSshKey`). Private keys are never stored (shown once on generate).
    use chrono::{DateTime, Utc};
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "AdSshKeys")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub participation_id: i32,
        pub algorithm: String,
        pub public_key: String,
        pub fingerprint: String,
        pub platform_generated: bool,
        pub created_at_utc: DateTime<Utc>,
        pub last_used_at_utc: Option<DateTime<Utc>>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

pub mod suspicion_rule {
    //! Admin-configurable weight for a suspicion detector rule code (RSCTF
    //! `SuspicionRule`). Absent rows fall back to the built-in default weight.
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "SuspicionRules")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        #[sea_orm(unique)]
        pub rule_code: String,
        pub weight: i32,
        pub description: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}
