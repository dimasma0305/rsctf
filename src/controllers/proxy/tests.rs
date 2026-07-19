use super::*;

#[test]
fn proxy_client_messages_have_a_small_memory_bound() {
    assert_eq!(MAX_CLIENT_MESSAGE_SIZE, 64 * 1024);
    const { assert!(MAX_CLIENT_MESSAGE_SIZE <= BUFFER_SIZE * 16) };
}

#[test]
fn proxy_close_frames_use_explicit_generic_codes() {
    let frame = |message| match message {
        Message::Close(Some(frame)) => frame,
        _ => panic!("expected an explicit WebSocket close frame"),
    };

    let normal = frame(normal_close());
    assert_eq!(normal.code, close_code::NORMAL);
    assert!(normal.reason.is_empty());

    let unavailable = frame(endpoint_unavailable_close());
    assert_eq!(unavailable.code, close_code::AGAIN);
    assert_eq!(unavailable.reason.as_str(), "proxy endpoint unavailable");

    let failed = frame(transport_failure_close());
    assert_eq!(failed.code, close_code::ERROR);
    assert_eq!(failed.reason.as_str(), "proxy transport failed");
}

fn exercise_row(user_id: Uuid) -> ExerciseAccessRow {
    ExerciseAccessRow {
        exercise_instance_id: 41,
        exercise_id: 9,
        user_id,
        is_loaded: true,
        is_enabled: true,
        publish_time_utc: chrono::Utc::now() - chrono::Duration::minutes(1),
    }
}

#[test]
fn exercise_access_requires_exact_live_owner_and_unambiguous_identity() {
    let owner = Uuid::new_v4();
    let now = chrono::Utc::now();
    let row = exercise_row(owner);
    assert_eq!(
        authorize_exercise_access(Some(41), owner, now, std::slice::from_ref(&row)),
        Some(ExerciseAccess {
            exercise_instance_id: 41,
            exercise_id: 9,
        })
    );
    assert!(authorize_exercise_access(None, owner, now, std::slice::from_ref(&row)).is_some());
    assert!(authorize_exercise_access(Some(42), owner, now, std::slice::from_ref(&row)).is_none());
    assert!(
        authorize_exercise_access(Some(41), Uuid::new_v4(), now, std::slice::from_ref(&row))
            .is_none()
    );
    assert!(authorize_exercise_access(Some(41), owner, now, &[row.clone(), row]).is_none());
}

#[test]
fn exercise_access_rejects_unloaded_disabled_and_unpublished_instances() {
    let owner = Uuid::new_v4();
    let now = chrono::Utc::now();
    let mut row = exercise_row(owner);
    row.is_loaded = false;
    assert!(authorize_exercise_access(Some(41), owner, now, &[row.clone()]).is_none());
    row.is_loaded = true;
    row.is_enabled = false;
    assert!(authorize_exercise_access(Some(41), owner, now, &[row.clone()]).is_none());
    row.is_enabled = true;
    row.publish_time_utc = now + chrono::Duration::minutes(1);
    assert!(authorize_exercise_access(Some(41), owner, now, &[row]).is_none());
}

#[test]
fn exercise_queries_bind_both_sides_and_keep_legacy_links_revocable() {
    assert!(EXERCISE_ACCESS_SQL.contains("instance.container_id = $1"));
    assert!(EXERCISE_ACCESS_SQL.contains("$2::INTEGER IS NULL OR instance.id = $2"));
    assert!(EXERCISE_LEASE_SQL.contains("container.id = instance.container_id"));
    assert!(EXERCISE_LEASE_SQL.contains("container.exercise_instance_id IS NULL"));
    assert!(EXERCISE_LEASE_SQL.contains("container.exercise_instance_id = instance.id"));
    assert!(LEGACY_EXERCISE_OWNER_SQL.contains("container_id = $1"));
}
