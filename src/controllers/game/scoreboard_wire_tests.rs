use axum::body::HttpBody;
use axum::http::{header, HeaderMap, StatusCode};
use bytes::Bytes;

use super::*;

const LOAD_PROFILE_TEAMS: i32 = 500;
const LOAD_PROFILE_CHALLENGES: i32 = 20;

fn maximum_load_profile_board() -> ScoreboardModel {
    let at = Utc::now();
    let challenges = (1..=LOAD_PROFILE_CHALLENGES)
        .map(|id| ChallengeInfo {
            id,
            title: format!("Challenge {id:02}"),
            category: ChallengeCategory::Misc,
            challenge_type: ChallengeType::StaticAttachment,
            score: 100,
            solved: LOAD_PROFILE_TEAMS,
            deadline: None,
            bloods: Vec::new(),
            disable_blood_bonus: true,
        })
        .collect();
    let items = (1..=LOAD_PROFILE_TEAMS)
        .map(|id| ScoreboardItem {
            id,
            name: format!("Maximum roster team {id:03}"),
            bio: None,
            division_id: Some(1),
            avatar: None,
            score: i64::from(LOAD_PROFILE_CHALLENGES) * 100,
            rank: id,
            division_rank: Some(id),
            last_submission_time: at,
            solved_challenges: (1..=LOAD_PROFILE_CHALLENGES)
                .map(|challenge_id| ChallengeItem {
                    id: challenge_id,
                    score: 100,
                    submission_type: SubmissionType::Normal,
                    user_name: Some(format!("player-{id:03}")),
                    time: at,
                })
                .collect(),
            solved_count: LOAD_PROFILE_CHALLENGES as usize,
        })
        .collect();
    ScoreboardModel {
        update_time_utc: at,
        blood_bonus: 0,
        timelines: Vec::new(),
        items,
        divisions: vec![serde_json::json!({ "id": 1, "name": "Open" })],
        challenges: BTreeMap::from([("Misc".to_owned(), challenges)]),
        challenge_count: LOAD_PROFILE_CHALLENGES,
        freeze: None,
        is_frozen_view: false,
    }
}

#[tokio::test]
async fn maximum_roster_profile_is_cacheable_precompressed_and_conditional() {
    let raw = Bytes::from(serde_json::to_vec(&maximum_load_profile_board()).unwrap());
    let built = super::super::scoreboard_encoding::build_stable_bundle(
        raw.clone(),
        "_ScoreBoardWireV2Frozen_load-profile".to_owned(),
        b"\"updateTimeUtc\":",
    )
    .await
    .unwrap();
    assert!(
        built.cacheable,
        "{LOAD_PROFILE_TEAMS}-team × {LOAD_PROFILE_CHALLENGES}-challenge bundle must fit the cache cap"
    );
    let bundle_bytes = built.bytes.len();

    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT_ENCODING, "gzip".parse().unwrap());
    let response =
        super::super::scoreboard_encoding::response(built.bytes.clone(), &headers).unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_ENCODING], "gzip");
    let etag = response.headers()[header::ETAG].clone();
    let encoded_bytes = response.body().size_hint().exact().unwrap() as usize;
    assert!(
        encoded_bytes < raw.len() / 5,
        "encoded={encoded_bytes}, raw={}",
        raw.len()
    );
    eprintln!(
        "standard-scoreboard-profile teams={LOAD_PROFILE_TEAMS} challenges={LOAD_PROFILE_CHALLENGES} rawBytes={} gzipBytes={encoded_bytes} cacheBundleBytes={bundle_bytes}",
        raw.len()
    );

    headers.insert(header::IF_NONE_MATCH, etag);
    let unchanged = super::super::scoreboard_encoding::response(built.bytes, &headers).unwrap();
    assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(unchanged.body().size_hint().exact(), Some(0));
}
