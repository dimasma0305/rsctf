//! Pure-logic unit tests for the ported crypto/flag/enum helpers — the parts
//! that don't need a database.

use rsctf::utils::codec::sha256_str;
use rsctf::utils::crypto_utils::{ct_eq, hash_password, verify_password};
use rsctf::utils::enums::{ChallengeType, Role};
use rsctf::utils::flag_generator::{generate_flag, team_hash_salt};
use sea_orm::ActiveEnum;

#[test]
fn constant_time_eq() {
    assert!(ct_eq("flag{abc}", "flag{abc}"));
    assert!(!ct_eq("flag{abc}", "flag{abd}"));
    assert!(!ct_eq("short", "longer"));
    assert!(ct_eq("", ""));
}

#[test]
fn password_hash_roundtrip() {
    let hash = hash_password("hunter2!").expect("hashing succeeds");
    assert!(verify_password("hunter2!", &hash));
    assert!(!verify_password("wrong", &hash));
    // Each hash is salted and therefore unique.
    let hash2 = hash_password("hunter2!").expect("hashing succeeds");
    assert_ne!(hash, hash2);
}

#[test]
fn sha256_matches_known_vector() {
    assert_eq!(
        sha256_str(""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn challenge_type_classification() {
    assert!(ChallengeType::StaticAttachment.is_static());
    assert!(!ChallengeType::DynamicContainer.is_static());
    assert!(ChallengeType::DynamicContainer.is_container());
    assert!(ChallengeType::AttackDefense.uses_ad_engine());
    assert!(ChallengeType::KingOfTheHill.is_king_of_the_hill());
    assert!(!ChallengeType::StaticAttachment.uses_ad_engine());
}

#[test]
fn flag_template_placeholders_expand() {
    let salt = team_hash_salt("private-key");
    let flag = generate_flag(Some("flag{[TEAM_HASH]}"), &salt);
    assert!(flag.starts_with("flag{"));
    assert!(!flag.contains("[TEAM_HASH]"));

    // GUID placeholder differs each call; empty template yields a random flag.
    let a = generate_flag(Some("flag{[GUID]}"), &salt);
    let b = generate_flag(Some("flag{[GUID]}"), &salt);
    assert_ne!(a, b);
    assert!(generate_flag(None, &salt).starts_with("flag{"));
}

#[test]
fn role_enum_wire_format_is_string() {
    // STORED as its integer value (i16 column via DeriveActiveEnum)...
    assert_eq!(Role::Admin.into_value(), 3);
    assert_eq!(Role::try_from_value(&1).unwrap(), Role::User);
    // ...but SERIALIZED on the wire as the variant name, because RSCTF's client
    // (Api.ts) declares Role as a string enum (`Role.Admin = "Admin"`). Emitting
    // the integer here is what bounced admins off the admin page.
    assert_eq!(serde_json::to_string(&Role::Admin).unwrap(), r#""Admin""#);
    assert_eq!(
        serde_json::from_str::<Role>(r#""Monitor""#).unwrap(),
        Role::Monitor
    );
}
