use super::*;

fn policy_sets(game: i32) -> PolicySets {
    PolicySets {
        peers: format!("rsv_p_{game}"),
        forward_targets: format!("rsv_f_{game}"),
        local_targets: format!("rsv_l_{game}"),
        nat_targets: format!("rsv_n_{game}"),
        cooldown_blocks: format!("rsv_c_{game}"),
    }
}

#[test]
fn firewall_plan_pairs_each_games_peer_target_and_cooldown_sets() {
    let routes = vec![ServiceRoute {
        network: "10.13.40.0/24".parse().unwrap(),
        interface: "eth0".to_string(),
        directly_connected: true,
        local_address: Some("10.13.40.2".parse().unwrap()),
    }];
    let sets = vec![policy_sets(10), policy_sets(11)];
    let forward: Vec<String> = forwarding_rule_plan("rsv_a_test", &sets, None)
        .into_iter()
        .map(|rule| rule.join(" "))
        .collect();
    assert!(forward.iter().any(|rule| rule.contains("-p tcp")
        && rule.contains("rsv_p_10 src")
        && rule.contains("rsv_f_10 dst,dst")));
    assert!(forward.iter().any(|rule| {
        rule.contains("-p udp")
            && rule.contains("rsv_p_10 src")
            && rule.contains("rsv_f_10 dst,dst")
    }));
    for protocol in ["tcp", "udp"] {
        assert!(forward.iter().any(|rule| {
            rule.contains(&format!("-p {protocol}"))
                && rule.contains("rsv_c_10 src,dst,dst")
                && rule.ends_with("-j DROP")
        }));
    }
    assert!(!forward
        .iter()
        .any(|rule| rule.contains("rsv_p_10") && rule.contains("rsv_f_11")));
    assert_eq!(forward.last().map(String::as_str), Some("-j DROP"));

    let input: Vec<String> = input_rule_plan("rsv_a_test", &sets, &routes, true, None)
        .into_iter()
        .map(|rule| rule.join(" "))
        .collect();
    assert!(input.iter().any(|rule| rule.contains("-p tcp")
        && rule.contains("rsv_p_10 src")
        && rule.contains("rsv_l_10 dst,dst")));
    assert!(input.iter().any(|rule| {
        rule.contains("-p udp")
            && rule.contains("rsv_p_10 src")
            && rule.contains("rsv_l_10 dst,dst")
    }));
    assert_eq!(input.last().map(String::as_str), Some("-j DROP"));

    let nat: Vec<String> = nat_rule_plan(&sets)
        .into_iter()
        .map(|rule| rule.join(" "))
        .collect();
    assert!(nat.iter().any(|rule| {
        rule.contains("-p tcp")
            && rule.contains("rsv_p_10 src")
            && rule.contains("rsv_n_10 dst,dst")
            && rule.contains("! -o wg0")
    }));
    assert!(nat.iter().any(|rule| {
        rule.contains("-p udp")
            && rule.contains("rsv_p_10 src")
            && rule.contains("rsv_n_10 dst,dst")
            && rule.contains("! -o wg0")
    }));
    assert_eq!(nat.last().map(String::as_str), Some("-j RETURN"));
}

#[test]
fn game_policy_set_names_are_short_and_stable() {
    let policy = GameVpnPolicy {
        game_id: 10,
        peers: vec!["10.13.0.5".parse().unwrap()],
        targets: vec![VpnTarget {
            address: "10.13.40.3".parse().unwrap(),
            port: 31337,
        }],
        cooldown_blocks: Vec::new(),
    };
    assert_eq!(
        policy_set_name("p", policy.game_id, "deadbeef"),
        "rsv_p_10_deadbeef"
    );
    assert!(policy_set_name("f", i32::MIN, "deadbeef").len() <= 31);
}

#[test]
fn quarantine_precedes_every_allow_rule() {
    let sets = vec![policy_sets(10)];
    let forward = forwarding_rule_plan("rsv_a_test", &sets, None);
    assert!(forward[1].join(" ").contains("rsv_quarantine src -j DROP"));
    assert!(
        forward
            .iter()
            .filter(|rule| rule
                .join(" ")
                .contains("rsv_transition src,dst,dst -j DROP"))
            .count()
            == 2
    );
    let first_forward_allow = forward
        .iter()
        .position(|rule| rule.last().is_some_and(|action| action == "ACCEPT"))
        .unwrap();
    assert!(first_forward_allow > 2);

    let input = input_rule_plan("rsv_a_test", &sets, &[], false, None);
    assert!(input[0].join(" ").contains("rsv_quarantine src -j DROP"));
    assert!(
        input
            .iter()
            .filter(|rule| rule
                .join(" ")
                .contains("rsv_transition src,dst,dst -j DROP"))
            .count()
            == 2
    );
    let first_input_allow = input
        .iter()
        .position(|rule| rule.last().is_some_and(|action| action == "ACCEPT"))
        .unwrap();
    assert!(first_input_allow > 1);
}

#[test]
fn same_origin_portal_opens_only_dns_and_https_for_live_peers() {
    let access = super::super::SameOriginAccess {
        dns: "10.13.42.1".parse().unwrap(),
        ingress: "10.13.41.253".parse().unwrap(),
    };
    let input = input_rule_plan("rsv_a_test", &[], &[], false, Some(access))
        .into_iter()
        .map(|rule| rule.join(" "))
        .collect::<Vec<_>>();
    assert!(input.iter().any(|rule| {
        rule.contains("-p udp")
            && rule.contains("rsv_a_test src")
            && rule.contains("-d 10.13.42.1 --dport 53")
            && rule.ends_with("-j ACCEPT")
    }));
    assert!(input.iter().any(|rule| {
        rule.contains("-p tcp")
            && rule.contains("rsv_a_test src")
            && rule.contains("-d 10.13.42.1 --dport 53")
            && rule.ends_with("-j ACCEPT")
    }));

    let forward = forwarding_rule_plan("rsv_a_test", &[], Some(access))
        .into_iter()
        .map(|rule| rule.join(" "))
        .collect::<Vec<_>>();
    assert!(forward.iter().any(|rule| {
        rule.contains("-i wg0 -p tcp")
            && rule.contains("rsv_a_test src")
            && rule.contains("-d 10.13.41.253 --dport 443")
            && rule.contains("NEW,ESTABLISHED")
    }));
    assert!(forward.iter().any(|rule| {
        rule.contains("-o wg0 -p tcp -s 10.13.41.253 --sport 443")
            && rule.contains("rsv_a_test dst")
            && rule.contains("ESTABLISHED,RELATED")
    }));
    assert!(!forward.iter().any(|rule| rule.contains("--dport 80")));
}

#[test]
fn installed_same_origin_rules_accept_iptables_canonical_argument_order() {
    let access = super::super::SameOriginAccess {
        dns: "10.13.42.1".parse().unwrap(),
        ingress: "10.13.41.253".parse().unwrap(),
    };
    let input = r#"
-A RSCTF_VPN_INPUT -d 10.13.42.1/32 -i wg0 -p udp -m set --match-set rsv_a_generation src -m udp --dport 53 -j ACCEPT
-A RSCTF_VPN_INPUT -d 10.13.42.1/32 -i wg0 -p tcp -m set --match-set rsv_a_generation src -m tcp --dport 53 -j ACCEPT
"#;
    let forward = r#"
-A RSCTF_VPN_FORWARD -d 10.13.41.253/32 -i wg0 -p tcp -m set --match-set rsv_a_generation src -m tcp --dport 443 -m conntrack --ctstate NEW,ESTABLISHED -j ACCEPT
-A RSCTF_VPN_FORWARD -s 10.13.41.253/32 -o wg0 -p tcp -m tcp --sport 443 -m set --match-set rsv_a_generation dst -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT
"#;
    assert!(same_origin_rule_text_installed(input, forward, access));

    assert!(!same_origin_rule_text_installed(
        &input.replace("--dport 53", "--dport 5353"),
        forward,
        access,
    ));
    assert!(!same_origin_rule_text_installed(
        input,
        &forward.replace("--dport 443", "--dport 4443"),
        access,
    ));
}

#[test]
fn route_parser_accepts_only_safe_linux_interface_names() {
    assert_eq!(
        parse_route_interface("10.13.40.1 dev eth0 src 10.13.40.2"),
        Some("eth0".to_string())
    );
    assert_eq!(
        parse_route_interface("10.13.40.1 dev cni0@if12 src 10.13.40.2"),
        Some("cni0".to_string())
    );
    assert_eq!(parse_route_interface("10.13.40.1 dev eth0;rm"), None);
}
