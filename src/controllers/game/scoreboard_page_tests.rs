use super::*;
use axum::http::{header, HeaderMap, StatusCode};

fn fixture() -> Snapshot {
    let at = Utc::now();
    Snapshot {
        version: "generation-one".into(),
        weight: 10_000,
        names: (1..=65).map(|id| format!("team {id:03}")).collect(),
        board: ScoreboardModel {
            update_time_utc: at,
            blood_bonus: 123,
            timelines: vec![serde_json::json!({ "id": 1, "items": [{"time": 1, "score": 120}] })],
            items: (1..=65)
                .map(|id| ScoreboardItem {
                    id,
                    name: format!("Team {id:03}"),
                    bio: None,
                    avatar: None,
                    division_id: Some(if id <= 35 { 1 } else { 2 }),
                    score: 1000 - i64::from(id),
                    rank: if id == 65 { 0 } else { id },
                    division_rank: Some(if id <= 35 { id } else { id - 35 }),
                    last_submission_time: at,
                    solved_challenges: Vec::new(),
                    solved_count: 0,
                })
                .collect(),
            divisions: vec![serde_json::json!({ "id": 1, "name": "Open" })],
            challenges: BTreeMap::new(),
            challenge_count: 0,
            freeze: Some(at),
            is_frozen_view: true,
        },
    }
}

#[test]
fn pages_are_bounded_and_preserve_the_canonical_ranks_and_frozen_metadata() {
    let snapshot = fixture();
    let query = ScoreboardPageQuery {
        skip: Some(30),
        ..Default::default()
    }
    .bounded()
    .unwrap();
    let page = project(&snapshot, &query);
    assert_eq!(
        (page.skip, page.count, page.total, page.items.len()),
        (30, 30, 64, 30)
    );
    assert_eq!(page.items[0].rank, 31);
    assert_eq!(page.items[29].rank, 60);
    assert!(page.is_frozen_view);
    assert_eq!(page.freeze, snapshot.board.freeze);
    assert_eq!(
        serde_json::to_value(page.timelines).unwrap(),
        serde_json::to_value(&snapshot.board.timelines).unwrap()
    );
    let wire = serde_json::to_value(&page).unwrap();
    assert_eq!(wire["snapshotVersion"], "generation-one");
    assert!(wire["updateTimeUtc"].is_i64());
    assert_eq!(
        wire["items"][0]["lastSubmissionTime"],
        snapshot.board.update_time_utc.timestamp_millis()
    );
}

#[test]
fn division_search_and_find_team_operate_over_the_entire_snapshot() {
    let snapshot = fixture();
    let query = ScoreboardPageQuery {
        count: Some(10),
        division_id: Some(2),
        search: Some("  TEAM 0 ".into()),
        locate_team: Some("Team 065".into()),
        ..Default::default()
    }
    .bounded()
    .unwrap();
    let page = project(&snapshot, &query);
    assert_eq!((page.total, page.skip, page.items.len()), (30, 20, 10));
    assert!(page.located);
    assert_eq!(page.items[9].id, 65);
    assert_eq!(page.items[9].rank, 0); // Division-only teams stay visible in their division.
    assert_eq!(page.items[9].division_rank, Some(30));
    let no_match = ScoreboardPageQuery {
        search: Some("no-such-team".into()),
        ..Default::default()
    }
    .bounded()
    .unwrap();
    let empty = project(&snapshot, &no_match);
    assert_eq!((empty.skip, empty.total), (0, 0));
    assert!(empty.items.is_empty());
}

#[test]
fn hostile_inputs_are_bounded_and_a_shrinking_roster_clamps_the_page() {
    for query in [
        ScoreboardPageQuery {
            skip: Some(100_001),
            ..Default::default()
        },
        ScoreboardPageQuery {
            search: Some("x".repeat(129)),
            ..Default::default()
        },
        ScoreboardPageQuery {
            locate_team: Some("x".repeat(257)),
            ..Default::default()
        },
    ] {
        assert!(query.bounded().is_err());
    }
    let query = ScoreboardPageQuery {
        count: Some(usize::MAX),
        skip: Some(100_000),
        ..Default::default()
    }
    .bounded()
    .unwrap();
    let snapshot = fixture();
    let page = project(&snapshot, &query);
    assert_eq!((page.skip, page.count, page.items.len()), (0, 100, 64));
    assert_eq!(
        ScoreboardPageQuery {
            count: Some(0),
            ..Default::default()
        }
        .bounded()
        .unwrap()
        .count,
        Some(1)
    );
}

#[tokio::test]
async fn validators_cover_query_generation_and_visibility_without_timestamp_churn() {
    let snapshot = fixture();
    let query = ScoreboardPageQuery::default().bounded().unwrap();
    let raw = Bytes::from(serde_json::to_vec(&project(&snapshot, &query)).unwrap());
    let bundle = encoding::build_stable_bundle(raw, "query-one".into(), b"\"updateTimeUtc\":")
        .await
        .unwrap()
        .bytes;
    let public =
        encoding::scoped_response(bundle.clone(), &HeaderMap::new(), "standard-public-page")
            .unwrap();
    let monitor =
        encoding::scoped_response(bundle.clone(), &HeaderMap::new(), "standard-monitor-page")
            .unwrap();
    assert_ne!(
        public.headers()[header::ETAG],
        monitor.headers()[header::ETAG]
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        header::IF_NONE_MATCH,
        public.headers()[header::ETAG].clone(),
    );
    assert_eq!(
        encoding::scoped_response(bundle.clone(), &headers, "standard-public-page")
            .unwrap()
            .status(),
        StatusCode::NOT_MODIFIED
    );
    assert_eq!(
        encoding::scoped_response(bundle, &headers, "standard-monitor-page")
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[test]
fn ctftime_contract_only_exports_ranked_rows_without_participant_details() {
    let snapshot = fixture();
    let standings = snapshot
        .board
        .items
        .iter()
        .filter(|item| item.rank > 0)
        .map(|item| CtftimeStanding {
            pos: item.rank,
            team: &item.name,
            score: item.score,
        })
        .collect();
    let wire = serde_json::to_value(CtftimeStandings { standings }).unwrap();
    assert_eq!(wire["standings"].as_array().unwrap().len(), 64);
    assert_eq!(
        wire["standings"][0],
        serde_json::json!({ "pos": 1, "team": "Team 001", "score": 999 })
    );
    assert!(wire.get("timelines").is_none());
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn postgres_pages_and_ctftime_preserve_public_monitor_and_unlisted_boundaries() {
    use super::super::ai_chats::tests::{body, fixture};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;
    let f = fixture().await;
    sqlx::query(
        r#"UPDATE "Games" SET freeze_time_utc = now() - interval '100 minutes',
        end_time_utc = now() + interval '1 hour', writeup_deadline = now() + interval '2 hours'
        WHERE id IN ($1, $2)"#,
    )
    .bind(f.open_game)
    .bind(f.off_game)
    .execute(&f.pool)
    .await
    .unwrap();
    super::super::scoreboard_board::invalidate_game_row_cache(f.open_game);
    super::super::scoreboard_board::invalidate_game_row_cache(f.off_game);

    for viewer in [
        None,
        Some(&f.outsider),
        Some(&f.pending),
        Some(&f.alice),
        Some(&f.admin),
    ] {
        let page = scoreboard_page(
            State(f.st.clone()),
            MaybeUser(viewer.cloned()),
            Path(f.open_game),
            Query(ScoreboardPageQuery {
                count: Some(100),
                ..Default::default()
            }),
            HeaderMap::new(),
        )
        .await
        .unwrap();
        let value = body(page).await;
        let canonical = body(
            super::super::scoreboard(
                State(f.st.clone()),
                MaybeUser(viewer.cloned()),
                Path(f.open_game),
                HeaderMap::new(),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(value["items"], canonical["items"]);
        assert_eq!(value["isFrozenView"], canonical["isFrozenView"]);
        assert_eq!(value["total"], 2);
        assert!(!value["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["name"] == "Gamma"));
        if viewer.is_some_and(|user| user.is_monitor()) {
            assert!(value["items"][0]["score"].as_i64().unwrap() > 0);
        } else {
            assert_eq!(value["items"][0]["score"], 0);
        }
    }
    let token =
        f.st.token
            .issue(
                f.admin.id,
                f.admin.role,
                &f.admin.name,
                &f.admin.security_stamp,
            )
            .unwrap();
    let response = super::super::router()
        .with_state(f.st.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/game/{}/scoreboard/ctftime", f.off_game))
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let export = body(response).await;
    assert!(export["standings"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["score"] == 0));
    assert!(
        ctftime_scoreboard(State(f.st.clone()), Path(f.open_game), HeaderMap::new())
            .await
            .is_err()
    );

    sqlx::query(r#"UPDATE "Games" SET hidden = TRUE WHERE id = $1"#)
        .bind(f.off_game)
        .execute(&f.pool)
        .await
        .unwrap();
    super::super::scoreboard_board::invalidate_game_row_cache(f.off_game);
    assert!(matches!(
        ctftime_scoreboard(State(f.st.clone()), Path(f.off_game), HeaderMap::new()).await,
        Err(AppError::NotFound(_))
    ));
    sqlx::query(
        r#"UPDATE "Games" SET freeze_time_utc = NULL, start_time_utc = now() + interval '1 hour',
        end_time_utc = now() + interval '2 hours', writeup_deadline = now() + interval '3 hours'
        WHERE id = $1"#,
    )
    .bind(f.open_game)
    .execute(&f.pool)
    .await
    .unwrap();
    super::super::scoreboard_board::invalidate_game_row_cache(f.open_game);
    assert!(scoreboard_page(
        State(f.st.clone()),
        MaybeUser(Some(f.admin.clone())),
        Path(f.open_game),
        Query(ScoreboardPageQuery::default()),
        HeaderMap::new()
    )
    .await
    .is_err());
    f.teardown().await;
}
