# Route, policy and regression navigation

This is a reviewed starting set, not an exhaustive authorization inventory.
Follow the backend policy even when a frontend control is hidden.
Tests named below must actually run; a source link does not establish a passing test.
The OAuth integration module also contains
`discord_browser_accepts_session_and_removes_only_state_cookie`, using
[Chromium without interception](../../../../tests/visual/oauth-cookie-browser.mjs).
It checks successful cross-site redirects, session attachment and rejection of an
unbound callback. A deployed TLS reverse-proxy smoke test is still required.

| Boundary | Route and handler | Backend policy | Regression and remaining evidence |
| --- | --- | --- | --- |
| OAuth session creation | [OAuth router and callback](../../../../src/controllers/oauth.rs): GET /api/oauth/{provider}/callback → callback → callback_with_provider | Browser-bound, one-use state; provider identity verification; account activation and registration policy. [Response cookies](../../../../src/controllers/oauth_response.rs) distinguish clearing state from issuing a session. | [Real provider/PostgreSQL test](../../../../src/controllers/oauth_integration_tests.rs): discord_split_cookies_issue_a_live_session_and_rejections_never_do. Real browser/proxy cookie acceptance remains a separate test requirement. |
| Durable job visibility and cancellation | [Editor router](../../../../src/controllers/edit/mod.rs): GET /api/edit/jobs/{jobId}, GET /api/edit/jobs/operations/{operationId}, POST /api/edit/jobs/{jobId} → [job handlers](../../../../src/controllers/edit/control_jobs.rs) | authorize_job validates the caller against the owning event. Operation identity is not authorization. | [Real HTTP router/PostgreSQL regression](../../../../src/controllers/api_contract/authorization_tests.rs): covered_routes_keep_anonymous_and_wrong_event_managers_out. Includes owner and cross-event access. |
| Player container create/delete/extend | [Game router](../../../../src/controllers/game/routes.rs) → [container handlers](../../../../src/controllers/game/containers.rs) | [Game context](../../../../src/services/game_authorization.rs), division permission, [current eligibility](../../../../src/services/player_containers/eligibility.rs), and exact expected container identity. Recheck ownership after waiting for locks. | [Delete/extend regressions](../../../../src/controllers/game/containers/delete_tests.rs), including stale_conditional_delete_cannot_destroy_a_real_replacement_runtime. Direct runtime execution remains; this is not proof of delegated web operation. |

For additional routes, add a row with registration, policy owner, negative test
and any missing end-to-end evidence. Do not substitute UI fixture authentication
for server-side permission tests. The navigation checker validates these links;
it does not parse Axum routing or prove policy completeness.
