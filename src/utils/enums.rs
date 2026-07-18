//! Domain enumerations, faithful to RSCTF `Utils/Enums.cs`.
//!
//! Each is stored in Postgres as a `SmallInteger` (the C# `byte`/`sbyte`) via
//! `DeriveActiveEnum`, but serialized ON THE WIRE as its STRING name — RSCTF's
//! React client (`Api.ts`) declares these as string enums (`Role.Admin =
//! "Admin"`, `ChallengeCategory.Misc = "Misc"`, …), so the JSON must be the
//! variant name, not the integer. The two numeric wire enums (`ReviewRating`,
//! `GamePermission`) use `db_enum_num!` / a transparent int instead.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

/// String-wire DB enum: `i16` column, JSON = the variant name (matches Api.ts).
macro_rules! db_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $val:literal),* $(,)? }) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash,
            EnumIter, DeriveActiveEnum, Serialize, Deserialize,
        )]
        #[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
        #[repr(i16)]
        pub enum $name {
            $($(#[$vmeta])* #[sea_orm(num_value = $val)] $variant = $val),*
        }
    };
}

/// Numeric-wire DB enum: `i16` column AND JSON = the integer (for the few enums
/// Api.ts declares numeric, e.g. `ReviewRating`).
macro_rules! db_enum_num {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $val:literal),* $(,)? }) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash,
            EnumIter, DeriveActiveEnum, Serialize_repr, Deserialize_repr,
        )]
        #[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
        #[repr(i16)]
        pub enum $name {
            $(#[sea_orm(num_value = $val)] $variant = $val),*
        }
    };
}

db_enum!(
    /// Global account role.
    Role { Banned = 0, User = 1, Monitor = 2, Admin = 3 }
);

db_enum!(
    ChallengeReviewStatus { Active = 0, Pending = 1, Rejected = 2 }
);

db_enum!(
    /// Jeopardy dynamic-score decay curve.
    ScoreCurve { Standard = 0, Linear = 1, Logarithmic = 2 }
);

db_enum!(
    ChallengeBuildStatus {
        None = 0, Success = 1, Failed = 2, Building = 3,
        NotApplicable = 4, Queued = 5, MissingDockerfile = 6
    }
);

db_enum!(
    RegisterStatus { LoggedIn = 0, AdminConfirmationRequired = 1, EmailConfirmationRequired = 2 }
);

db_enum!(
    FileType { None = 0, Local = 1, Remote = 2 }
);

db_enum!(
    ContainerStatus { Pending = 0, Running = 1, Destroyed = 2 }
);

db_enum!(
    NoticeType {
        Normal = 0, FirstBlood = 1, SecondBlood = 2, ThirdBlood = 3,
        NewHint = 4, NewChallenge = 5
    }
);

db_enum!(
    EventType {
        Normal = 0, ContainerStart = 1, ContainerDestroy = 2, FlagSubmit = 3,
        CheatDetected = 4, Download = 5, ChallengeOpened = 6
    }
);

db_enum!(
    SubmissionType { Unaccepted = 0, FirstBlood = 1, SecondBlood = 2, ThirdBlood = 3, Normal = 4 }
);

db_enum!(
    ParticipationStatus { Pending = 0, Accepted = 1, Rejected = 2, Suspended = 3, Unsubmitted = 4 }
);

db_enum!(
    /// Bit-encoded challenge kind. See `ChallengeType::*` helper methods.
    ChallengeType {
        StaticAttachment = 0,
        StaticContainer = 1,
        DynamicAttachment = 2,
        DynamicContainer = 3,
        AttackDefense = 4,
        KingOfTheHill = 5,
    }
);

impl ChallengeType {
    pub fn is_static(self) -> bool {
        matches!(self, Self::StaticAttachment | Self::StaticContainer)
    }
    pub fn is_dynamic(self) -> bool {
        !self.is_static()
    }
    pub fn is_attachment(self) -> bool {
        matches!(self, Self::StaticAttachment | Self::DynamicAttachment)
    }
    pub fn is_container(self) -> bool {
        matches!(
            self,
            Self::StaticContainer
                | Self::DynamicContainer
                | Self::AttackDefense
                | Self::KingOfTheHill
        )
    }
    pub fn is_attack_defense(self) -> bool {
        matches!(self, Self::AttackDefense)
    }
    pub fn is_king_of_the_hill(self) -> bool {
        matches!(self, Self::KingOfTheHill)
    }
    pub fn uses_ad_engine(self) -> bool {
        matches!(self, Self::AttackDefense | Self::KingOfTheHill)
    }
}

db_enum!(
    AdCheckStatus { Ok = 0, Mumble = 1, Offline = 2, InternalError = 3 }
);

db_enum!(
    ChallengeCategory {
        Misc = 0, Crypto = 1, Pwn = 2, Web = 3, Reverse = 4, Blockchain = 5,
        Forensics = 6, Hardware = 7, Mobile = 8,
        #[serde(rename = "PPC")] Ppc = 9,
        #[serde(rename = "AI")] Ai = 10,
        Pentest = 11,
        #[serde(rename = "OSINT")] Osint = 12
    }
);

db_enum!(
    NetworkMode { Open = 0, Isolated = 32, Custom = 255 }
);

db_enum!(
    /// Flag-submission outcome. Negative `NotFound` matches the C# `sbyte`.
    AnswerResult { NotFound = -1, FlagSubmitted = 0, Accepted = 1, WrongAnswer = 2, CheatDetected = 3 }
);

db_enum_num!(
    /// Numeric on the wire in Api.ts (`ReviewRating = 0`).
    ReviewRating { None = 0, Poor = 1, Fair = 2, Good = 3, Excellent = 4 }
);

db_enum!(
    RepoWatchStatus { Active = 0, Paused = 1 }
);

/// Task/health status (`sbyte` in C#). String on the wire (Api.ts
/// `TaskStatus = "Success"`); never persisted as a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Unhealthy = -3,
    Degraded = -2,
    Pending = -1,
    Success = 0,
    Failed = 1,
    Duplicate = 2,
    Denied = 3,
    NotFound = 4,
    Exit = 5,
}

/// Bit-flag game permissions. Stored as an `i32` column; exposed with the
/// same semantics as the C# `[Flags] enum GamePermission`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GamePermission(pub i32);

impl GamePermission {
    pub const JOIN_GAME: i32 = 1 << 0;
    pub const RANK_OVERALL: i32 = 1 << 1;
    pub const REQUIRE_REVIEW: i32 = 1 << 2;
    pub const VIEW_CHALLENGE: i32 = 1 << 8;
    pub const SUBMIT_FLAGS: i32 = 1 << 9;
    pub const GET_SCORE: i32 = 1 << 10;
    pub const GET_BLOOD: i32 = 1 << 11;
    pub const AFFECT_DYNAMIC_SCORE: i32 = 1 << 12;
    pub const ALL: i32 = i32::MAX;

    pub fn contains(self, flag: i32) -> bool {
        self.0 & flag == flag
    }
}

impl Default for GamePermission {
    fn default() -> Self {
        GamePermission(Self::ALL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The React client (`Api.ts`) declares these as STRING enums, so the JSON
    /// wire form MUST be the variant name (not the integer). This is the contract
    /// whose breakage bounced admins off the admin page (`RoleMap.get(3)` = undef).
    #[test]
    fn string_enums_serialize_as_names() {
        assert_eq!(serde_json::to_string(&Role::Admin).unwrap(), r#""Admin""#);
        assert_eq!(
            serde_json::to_string(&Role::Monitor).unwrap(),
            r#""Monitor""#
        );
        assert_eq!(
            serde_json::to_string(&ChallengeCategory::Misc).unwrap(),
            r#""Misc""#
        );
        // Acronym categories are uppercased in Api.ts.
        assert_eq!(
            serde_json::to_string(&ChallengeCategory::Ppc).unwrap(),
            r#""PPC""#
        );
        assert_eq!(
            serde_json::to_string(&ChallengeCategory::Ai).unwrap(),
            r#""AI""#
        );
        assert_eq!(
            serde_json::to_string(&ChallengeCategory::Osint).unwrap(),
            r#""OSINT""#
        );
        assert_eq!(
            serde_json::to_string(&ChallengeType::DynamicContainer).unwrap(),
            r#""DynamicContainer""#
        );
        assert_eq!(
            serde_json::to_string(&AnswerResult::Accepted).unwrap(),
            r#""Accepted""#
        );
        assert_eq!(
            serde_json::to_string(&NoticeType::FirstBlood).unwrap(),
            r#""FirstBlood""#
        );
        assert_eq!(
            serde_json::to_string(&ParticipationStatus::Accepted).unwrap(),
            r#""Accepted""#
        );
        assert_eq!(
            serde_json::to_string(&TaskStatus::Success).unwrap(),
            r#""Success""#
        );
        // Round-trip from the string form the client sends.
        assert_eq!(
            serde_json::from_str::<Role>(r#""Admin""#).unwrap(),
            Role::Admin
        );
        assert_eq!(
            serde_json::from_str::<ChallengeCategory>(r#""PPC""#).unwrap(),
            ChallengeCategory::Ppc
        );
    }

    /// The two Api.ts numeric enums stay integers on the wire.
    #[test]
    fn numeric_enums_stay_integers() {
        assert_eq!(serde_json::to_string(&ReviewRating::None).unwrap(), "0");
        assert_eq!(serde_json::to_string(&ReviewRating::Good).unwrap(), "3");
    }
}
