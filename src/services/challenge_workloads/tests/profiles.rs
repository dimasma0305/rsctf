use super::*;

#[test]
fn placement_save_preserves_existing_workload_but_fences_in_flight_launches() {
    let mut challenge = legacy_challenge();
    let image = "registry.example/app@sha256:fixture".to_string();
    let identity = legacy_runtime_identity(&challenge, image.clone()).unwrap();
    let original = runtime_publication_fence(&challenge, &identity).unwrap();
    for profile in ["docker", "kubernetes", "microvm"] {
        challenge.runtime_profile = Some(profile.into());
        assert_eq!(
            legacy_runtime_identity(&challenge, image.clone()).unwrap(),
            identity
        );
        assert_ne!(
            runtime_publication_fence(&challenge, &identity).unwrap(),
            original
        );
    }
}
