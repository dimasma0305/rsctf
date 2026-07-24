fn compact(source: &str) -> String {
    source
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>()
        .replace(",)", ")")
}

#[test]
fn every_broadcast_hub_entry_point_is_source_rate_limited() {
    for (source, routes) in [
        (
            compact(include_str!("attack.rs")),
            vec![
                (
                    "/hub/attack",
                    "limited(Policy::PublicHubAdmission,get(attack_hub))",
                ),
                (
                    "/hub/attack/negotiate",
                    "limited(Policy::PublicHubAdmission,post(signalr::negotiate))",
                ),
                (
                    "/hub/attack/ws",
                    "limited(Policy::PublicHubAdmission,get(attack_ws))",
                ),
            ],
        ),
        (
            compact(include_str!("user.rs")),
            vec![
                (
                    "/hub/user",
                    "limited(Policy::PublicHubAdmission,get(user_hub))",
                ),
                (
                    "/hub/user/negotiate",
                    "limited(Policy::PublicHubAdmission,post(signalr::negotiate))",
                ),
            ],
        ),
        (
            compact(include_str!("monitor.rs")),
            vec![
                (
                    "/hub/monitor",
                    "limited(Policy::PrivilegedHubAdmission,get(monitor_hub))",
                ),
                (
                    "/hub/monitor/negotiate",
                    "limited(Policy::PrivilegedHubAdmission,post(signalr::monitor_negotiate))",
                ),
            ],
        ),
        (
            compact(include_str!("admin.rs")),
            vec![
                (
                    "/hub/admin",
                    "limited(Policy::PrivilegedHubAdmission,get(admin_hub))",
                ),
                (
                    "/hub/admin/negotiate",
                    "limited(Policy::PrivilegedHubAdmission,post(signalr::admin_negotiate))",
                ),
            ],
        ),
    ] {
        for (path, handler) in routes {
            assert!(
                source.contains(&format!(".route(\"{path}\",{handler})")),
                "{path} is missing source-IP hub admission"
            );
        }
    }
}

#[test]
fn every_broadcast_upgrade_applies_transport_and_connection_limits() {
    for (name, source) in [
        ("attack", include_str!("attack.rs")),
        ("user", include_str!("user.rs")),
        ("monitor", include_str!("monitor.rs")),
        ("admin", include_str!("admin.rs")),
    ] {
        assert!(
            source.contains("signalr::bounded_upgrade(ws)"),
            "{name} hub is missing WebSocket transport limits"
        );
        assert!(
            source.contains("admission::try_connection_permit("),
            "{name} hub is missing retained-connection admission"
        );
    }
}

#[test]
fn public_visibility_heartbeats_use_the_single_flight_game_cache() {
    let source = include_str!("signalr.rs");
    assert!(source.contains("crate::controllers::game::load_game_cached"));
    assert!(!source.contains("game::Entity::find_by_id"));
}
