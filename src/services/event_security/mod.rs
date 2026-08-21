//! Event-scoped VPN access, bounded telemetry, and evidence fusion.
//!
//! All event switches default off. The access gate is enforced only during
//! `[start_time_utc, end_time_utc)`; practice and archived views keep their
//! existing public behavior.

mod challenge_policy;
mod fusion;
mod peer;
mod policy;
mod proof;
mod receipts;
mod sensor_contract;
mod telemetry;
mod variants;

pub use challenge_policy::*;
pub use fusion::*;
pub use peer::*;
pub use policy::*;
pub use proof::*;
pub use receipts::*;
pub use sensor_contract::*;
pub use telemetry::*;
pub use variants::*;
