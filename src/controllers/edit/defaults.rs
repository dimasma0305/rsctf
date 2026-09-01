//! Compatibility defaults shared by editor request models and game mutations.

use chrono::{DateTime, Utc};

const BLOOD_BONUS_DEFAULT: i64 = (50 << 20) + (30 << 10) + 10;

/// Port of RSCTF `BloodBonus.FromValue`: a packed value whose any of the three
/// 10-bit fields exceeds 1000 is rejected in favor of the compatibility default.
pub(super) fn blood_bonus_from_value(value: i64) -> i64 {
    const MASK: i64 = 0x3ff;
    const BASE: i64 = 1000;
    if (value & MASK) > BASE || ((value >> 10) & MASK) > BASE || ((value >> 20) & MASK) > BASE {
        BLOOD_BONUS_DEFAULT
    } else {
        value
    }
}

pub(super) fn epoch() -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(0, 0).expect("unix epoch is a valid timestamp")
}

pub(super) fn default_true() -> bool {
    true
}

pub(super) fn default_container_limit() -> i32 {
    3
}

pub(super) fn default_blood_bonus() -> i64 {
    BLOOD_BONUS_DEFAULT
}
