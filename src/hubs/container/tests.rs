use super::*;

const _: () = {
    assert!(MAX_SESSIONS_PER_CONNECTION > 0);
    assert!(MAX_PACKED_INVOCATIONS >= MAX_SESSIONS_PER_CONNECTION);
    assert!(MAX_INPUT_BASE64_BYTES < MAX_WS_MESSAGE_BYTES);
    assert!(MAX_OUTPUT_CHUNK_BYTES < MAX_WS_MESSAGE_BYTES);
    assert!(EXEC_INPUT_QUEUE > 0);
    assert!(EXEC_OUTPUT_QUEUE > EXEC_INPUT_QUEUE);
};

#[test]
fn packed_signalr_frames_are_bounded_before_dispatch() {
    let message = format!("{{\"type\":6}}{RS}");
    assert_eq!(packed_invocation_count(""), Some(0));
    assert_eq!(packed_invocation_count(&message.repeat(64)), Some(64));
    assert_eq!(packed_invocation_count(&message.repeat(65)), None);
}

#[test]
fn terminal_transport_limits_are_internally_consistent() {
    assert_eq!(bounded_tty_dimension(Some(u64::MAX), 80, 20, 500), 500);
    assert_eq!(bounded_tty_dimension(Some(0), 80, 20, 500), 20);
    assert_eq!(bounded_tty_dimension(None, 80, 20, 500), 80);
    assert_eq!(bounded_input("%%%"), Err("Malformed container input"));
    assert_eq!(
        bounded_input(&"A".repeat(MAX_INPUT_BASE64_BYTES + 1)),
        Err("Container input limit exceeded")
    );

    let per_connection_input = MAX_SESSIONS_PER_CONNECTION * EXEC_INPUT_QUEUE * MAX_INPUT_BYTES;
    let encoded_output = EXEC_OUTPUT_QUEUE * (MAX_OUTPUT_CHUNK_BYTES * 4 / 3 + 512);
    assert!(per_connection_input + encoded_output < 2 * 1024 * 1024);
}

#[test]
fn every_exec_admission_route_is_ip_rate_limited() {
    let compact = include_str!("../container.rs")
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .replace(",)", ")");
    for (path, method) in [
        ("/hub/containerExec", "get(container_hub)"),
        (
            "/hub/containerExec/negotiate",
            "post(signalr::admin_negotiate)",
        ),
        (
            "/hub/containerExec/games/{game_id}",
            "get(scoped_container_hub)",
        ),
        (
            "/hub/containerExec/games/{game_id}/negotiate",
            "post(scoped_negotiate)",
        ),
    ] {
        assert!(compact.contains(&format!(
            ".route(\"{path}\",limited(Policy::PrivilegedHubAdmission,{method})"
        )));
    }
}
