//! Constructing a router runs every controller's route registration, and axum
//! panics on conflicting routes. This test builds the full merged router (and
//! each controller in isolation), so route collisions are caught in CI without
//! needing a database or a running server.

#[test]
fn api_router_builds_without_route_conflicts() {
    // Panics here (E.g. "Overlapping method route") if any two routes collide.
    let _ = rsctf::server::api_router();
    let _ = rsctf::server::web_api_router();
    let _ = rsctf::server::stateful_api_router();
}

#[test]
fn each_controller_router_builds() {
    let _ = rsctf::controllers::account::router();
    let _ = rsctf::controllers::team::router();
    let _ = rsctf::controllers::game::router();
    let _ = rsctf::controllers::game::web_router();
    let _ = rsctf::controllers::edit::router();
    let _ = rsctf::controllers::admin::router();
    let _ = rsctf::controllers::info::router();
    let _ = rsctf::controllers::assets::router();
    let _ = rsctf::controllers::api_token::router();
    let _ = rsctf::controllers::workers::router();
    let _ = rsctf::controllers::game::ad::router();
    let _ = rsctf::controllers::game::ad::web_router();
    let _ = rsctf::controllers::game::ad::stateful_router();
    let _ = rsctf::controllers::admin::ad::router();
    let _ = rsctf::hubs::monitor::router();
    let _ = rsctf::hubs::user::router();
    let _ = rsctf::hubs::admin::router();
}
