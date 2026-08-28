use super::*;

pub(super) fn router() -> Router<SharedState> {
    router_with_domains(ad::router(), koth::router())
}

pub(super) fn web_router() -> Router<SharedState> {
    router_with_domains(ad::web_router(), koth::web_router())
}

fn router_with_domains(
    ad_router: Router<SharedState>,
    koth_router: Router<SharedState>,
) -> Router<SharedState> {
    Router::new()
        .route("/api/game", limited(Policy::Query, get(games)))
        .route(
            "/api/game/challenges",
            limited(Policy::Query, get(challenge_catalog)),
        )
        .route(
            "/api/game/recent",
            limited(Policy::Query, get(recent_games)),
        )
        .route(
            "/api/game/{id}",
            get(game_details).post(join_game).delete(leave_game),
        )
        .route("/api/game/{id}/details", get(game_details_with_challenges))
        .route(
            "/api/game/{id}/details/catalog",
            get(game_challenge_catalog),
        )
        .route("/api/game/{id}/details/live", get(game_participant_delta))
        .route("/api/game/{id}/notices", get(notices))
        .route("/api/game/{id}/events", limited(Policy::Query, get(events)))
        .route(
            "/api/game/{id}/events/page",
            limited(Policy::Query, get(monitor_history::event_page)),
        )
        .route(
            "/api/game/{id}/events/backfill",
            limited(Policy::Query, get(event_backfill)),
        )
        .route(
            "/api/game/{id}/participations",
            limited(Policy::Query, get(participations)),
        )
        .route(
            "/api/game/{id}/participations/page",
            limited(Policy::Query, get(participation_page)),
        )
        .route(
            "/api/game/{id}/participations/{participationId}",
            limited(Policy::Query, get(participation_detail)),
        )
        // The scoreboard is fully cache-served (cheap), so the always-on Global
        // window is protection enough — dropping the per-route Query decorator
        // halves the limiter work on the single hottest endpoint. A deliberate
        // divergence from RSCTF, which keeps a Query limit here.
        .route("/api/game/{id}/scoreboard", get(scoreboard))
        .route(
            "/api/game/{id}/scoreboard/combined",
            get(combined_scoreboard),
        )
        .route("/api/game/{id}/scoreboardsheet", get(scoreboard_sheet))
        .route(
            "/api/game/{id}/submissions",
            limited(Policy::Query, get(submissions)),
        )
        .route(
            "/api/game/{id}/submissions/page",
            limited(Policy::Query, get(monitor_history::submission_page)),
        )
        .route(
            "/api/game/{id}/submissions/backfill",
            limited(Policy::Query, get(submission_backfill)),
        )
        .route("/api/game/{id}/submissionsheet", get(submission_sheet))
        .route("/api/game/{id}/check", get(join_check))
        .route("/api/game/{id}/vpn/challenge", post(vpn_challenge))
        .route("/api/game/{id}/vpn/proof", post(vpn_proof))
        .route("/api/game/{id}/vpn/config", get(vpn_config))
        .route(
            "/api/game/{id}/cheatinfo",
            limited(Policy::Query, get(cheat_info)),
        )
        .route(
            "/api/game/{id}/cheatreport",
            limited(Policy::Query, get(cheat_report)),
        )
        .route(
            "/api/game/{id}/cheatreport/events/{eventId}",
            limited(Policy::Query, get(suspicion_event_evidence)),
        )
        .route(
            "/api/game/{id}/cheatreport/compare",
            limited(Policy::Query, get(cheat_report_compare)),
        )
        .route(
            "/api/game/{id}/writeup",
            get(get_writeup).merge(post(submit_writeup).layer(DefaultBodyLimit::max(
                crate::utils::upload::WRITEUP_BODY_BYTES,
            ))),
        )
        .route(
            "/api/game/{id}/challenge/{challengeId}/open",
            post(open_challenge),
        )
        .route(
            "/api/game/{id}/challenges/{challengeId}/solvers",
            get(challenge_solvers),
        )
        .route(
            "/api/game/{id}/challenges/{challengeId}/solvers/page",
            get(challenge_solver_page),
        )
        .route(
            "/api/game/{id}/challenges/{challengeId}",
            // Only the POST (flag submit) carries the Submit policy, like RSCTF's
            // per-action [EnableRateLimiting]; the GET detail is unthrottled.
            get(get_challenge).merge(limited(
                Policy::Submit,
                post(submit).layer(DefaultBodyLimit::max(8 * 1024)),
            )),
        )
        .route(
            "/api/game/{id}/challenges/{challengeId}/review",
            post(review_challenge),
        )
        .route(
            "/api/game/{id}/challenges/{challengeId}/status/{submitId}",
            limited(Policy::Verdict, get(status)),
        )
        .route(
            "/api/game/{id}/container/{challengeId}",
            limited(
                Policy::Container,
                post(create_container).delete(delete_container),
            ),
        )
        .route(
            "/api/game/{id}/container/{challengeId}/extend",
            limited(Policy::Container, post(extend_container)),
        )
        // Traffic capture subsystem — registered, well-typed empty payloads.
        .route(
            "/api/game/games/{id}/captures",
            limited(Policy::Query, get(game_captures)),
        )
        .route(
            "/api/game/captures/{challengeId}",
            limited(Policy::Query, get(team_traffic)),
        )
        .route(
            "/api/game/captures/{challengeId}/{partId}",
            limited(Policy::Query, get(traffic_files)),
        )
        .route(
            "/api/game/captures/{challengeId}/{partId}/all",
            limited(Policy::Query, get(get_all_traffic)).delete(delete_all_traffic),
        )
        .route(
            "/api/game/captures/{challengeId}/{partId}/{filename}",
            get(get_traffic_file).delete(delete_traffic_file),
        )
        .route(
            "/api/game/captures/{challengeId}/{partId}/{filename}/flows",
            limited(Policy::Query, get(traffic_flows)),
        )
        .route(
            "/api/game/captures/{challengeId}/{partId}/{filename}/flow/{connectionPort}",
            limited(Policy::Query, get(traffic_flow_detail)),
        )
        // Player-facing A&D + KotH controllers live under this game area.
        .merge(ad_router)
        .merge(koth_router)
}
