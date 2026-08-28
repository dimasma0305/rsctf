use super::ChallengeUpdateModel;

#[test]
fn workload_update_distinguishes_missing_from_null() {
    let unchanged: ChallengeUpdateModel = serde_json::from_str("{}").unwrap();
    assert!(unchanged.workload_spec.is_none());

    let cleared: ChallengeUpdateModel = serde_json::from_str(r#"{"workloadSpec":null}"#).unwrap();
    assert!(matches!(cleared.workload_spec, Some(None)));
}
