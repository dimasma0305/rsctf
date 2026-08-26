use super::*;

fn challenge(id: i32, category: ChallengeCategory) -> ChallengeInfo {
    ChallengeInfo {
        id,
        title: format!("challenge-{id}"),
        category,
        challenge_type: ChallengeType::StaticAttachment,
        score: 100,
        solved: 0,
        deadline: None,
        bloods: Vec::new(),
        disable_blood_bonus: true,
    }
}

fn solve(id: i32) -> ChallengeItem {
    ChallengeItem {
        id,
        score: 100,
        submission_type: SubmissionType::Normal,
        user_name: None,
        time: DateTime::<Utc>::MIN_UTC,
    }
}

#[test]
fn details_count_visible_challenges_and_filter_rank_solves_to_the_same_ids() {
    let mut challenges = BTreeMap::new();
    challenges.insert(
        "Misc".to_owned(),
        vec![
            challenge(1, ChallengeCategory::Misc),
            challenge(2, ChallengeCategory::Misc),
        ],
    );
    challenges.insert("Web".to_owned(), vec![challenge(3, ChallengeCategory::Web)]);
    let visible_ids = visible_challenge_ids(&challenges);
    let visible: HashSet<i32> = visible_ids.iter().copied().collect();
    let mut rank = ScoreboardItem {
        id: 7,
        name: "team".to_owned(),
        bio: None,
        division_id: Some(4),
        avatar: None,
        score: 300,
        rank: 1,
        division_rank: Some(1),
        last_submission_time: DateTime::<Utc>::MIN_UTC,
        solved_challenges: vec![solve(1), solve(3), solve(99)],
        solved_count: 3,
    };
    retain_visible_solves(&mut rank, &visible);
    let model = GameDetailModel {
        challenge_count: i32::try_from(visible_ids.len()).unwrap(),
        challenges,
        rank: Some(rank),
        team_token: "redacted-test-token".to_owned(),
        writeup_required: false,
        writeup_deadline: DateTime::<Utc>::MIN_UTC,
    };

    let wire = serde_json::to_value(model).unwrap();
    assert_eq!(wire["challengeCount"], 3);
    assert_eq!(wire["rank"]["solvedCount"], 2);
    assert_eq!(
        wire["rank"]["solvedChallenges"].as_array().unwrap().len(),
        2
    );
}

#[test]
fn empty_visible_projection_has_zero_challenges_and_solves() {
    let challenges = BTreeMap::<String, Vec<ChallengeInfo>>::new();
    assert!(visible_challenge_ids(&challenges).is_empty());
}

#[test]
fn division_permission_change_reprojects_the_next_details_response() {
    let mut challenges = BTreeMap::new();
    challenges.insert(
        "Misc".to_owned(),
        vec![
            challenge(1, ChallengeCategory::Misc),
            challenge(2, ChallengeCategory::Misc),
        ],
    );
    let mut rank = ScoreboardItem {
        id: 7,
        name: "team".to_owned(),
        bio: None,
        division_id: Some(4),
        avatar: None,
        score: 200,
        rank: 1,
        division_rank: Some(1),
        last_submission_time: DateTime::<Utc>::MIN_UTC,
        solved_challenges: vec![solve(1), solve(2)],
        solved_count: 2,
    };

    // A later poll sees challenge 1 revoked for the division. Its count and
    // solve numerator must both derive from that new visible projection.
    challenges.get_mut("Misc").unwrap().remove(0);
    let visible_ids = visible_challenge_ids(&challenges);
    let visible: HashSet<i32> = visible_ids.iter().copied().collect();
    retain_visible_solves(&mut rank, &visible);

    assert_eq!(visible_ids, vec![2]);
    assert_eq!(rank.solved_count, 1);
    assert_eq!(rank.solved_challenges[0].id, 2);
}
