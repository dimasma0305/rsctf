use super::*;

pub(super) fn router() -> Router<SharedState> {
    router_with_ad(ad::router())
}

pub(super) fn web_router() -> Router<SharedState> {
    router_with_ad(ad::web_router())
}

fn router_with_ad(ad_router: Router<SharedState>) -> Router<SharedState> {
    Router::new()
        .route("/api/game", limited(Policy::Query, get(games)))
        .route(
            "/api/game/recent",
            limited(Policy::Query, get(recent_games)),
        )
        .route(
            "/api/game/{id}",
            get(game_details).post(join_game).delete(leave_game),
        )
        .route("/api/game/{id}/details", get(game_details_with_challenges))
        .route("/api/game/{id}/notices", get(notices))
        .route("/api/game/{id}/events", get(events))
        .route("/api/game/{id}/participations", get(participations))
        // The scoreboard is fully cache-served (cheap), so the always-on Global
        // window is protection enough — dropping the per-route Query decorator
        // halves the limiter work on the single hottest endpoint. A deliberate
        // divergence from RSCTF, which keeps a Query limit here.
        .route("/api/game/{id}/scoreboard", get(scoreboard))
        .route("/api/game/{id}/scoreboardsheet", get(scoreboard_sheet))
        .route("/api/game/{id}/submissions", get(submissions))
        .route("/api/game/{id}/submissionsheet", get(submission_sheet))
        .route("/api/game/{id}/check", get(join_check))
        .route("/api/game/{id}/cheatinfo", get(cheat_info))
        .route("/api/game/{id}/cheatreport", get(cheat_report))
        .route(
            "/api/game/{id}/cheatreport/compare",
            get(cheat_report_compare),
        )
        .route(
            "/api/game/{id}/writeup",
            get(get_writeup).post(submit_writeup),
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
            "/api/game/{id}/challenges/{challengeId}",
            // Only the POST (flag submit) carries the Submit policy, like RSCTF's
            // per-action [EnableRateLimiting]; the GET detail is unthrottled.
            get(get_challenge).merge(limited(Policy::Submit, post(submit))),
        )
        .route(
            "/api/game/{id}/challenges/{challengeId}/review",
            post(review_challenge),
        )
        .route(
            "/api/game/{id}/challenges/{challengeId}/status/{submitId}",
            get(status),
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
        .route("/api/game/games/{id}/captures", get(game_captures))
        .route("/api/game/captures/{challengeId}", get(team_traffic))
        .route(
            "/api/game/captures/{challengeId}/{partId}",
            get(traffic_files),
        )
        .route(
            "/api/game/captures/{challengeId}/{partId}/all",
            get(get_all_traffic).delete(delete_all_traffic),
        )
        .route(
            "/api/game/captures/{challengeId}/{partId}/{filename}",
            get(get_traffic_file).delete(delete_traffic_file),
        )
        .route(
            "/api/game/captures/{challengeId}/{partId}/{filename}/flows",
            get(traffic_flows),
        )
        .route(
            "/api/game/captures/{challengeId}/{partId}/{filename}/flow/{connectionPort}",
            get(traffic_flow_detail),
        )
        // Player-facing A&D + KotH controllers live under this game area.
        .merge(ad_router)
        .merge(koth::router())
}
