use super::*;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn credential_publication_tracks_the_last_committed_import() {
    use crate::controllers::admin::users_credentials::{
        credential_cache_key, CachedImportCredential,
    };

    let harness = Harness::new().await;
    let email = "race@example.test";
    let normalized_email = "RACE@EXAMPLE.TEST";
    let first = provision_import_user(
        &harness.pool,
        import_write(normalized_email, "first-hash"),
        credential_write(&harness, "first-password"),
        None,
        None,
    )
    .await
    .unwrap();
    let ImportProvision::Provisioned(first) = first else {
        panic!("first import was unexpectedly skipped");
    };
    let second = provision_import_user(
        &harness.pool,
        import_write(normalized_email, "second-hash"),
        credential_write(&harness, "second-password"),
        None,
        None,
    )
    .await
    .unwrap();
    let ImportProvision::Provisioned(second) = second else {
        panic!("second import was unexpectedly skipped");
    };
    assert_eq!(first.id, second.id);
    assert_ne!(first.security_stamp, second.security_stamp);

    let value = harness
        .cache
        .get(&credential_cache_key(email))
        .await
        .expect("newest credential disappeared");
    let credential: CachedImportCredential = serde_json::from_slice(&value).unwrap();
    assert_eq!(credential.user_id, second.id);
    assert_eq!(credential.security_stamp, second.security_stamp);
    assert_eq!(credential.password, "second-password");
    harness.cleanup().await;
}
