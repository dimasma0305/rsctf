use super::super::*;
use super::import_with_game_lock;
use chrono::DateTime;

#[test]
fn score_curve_manifest_accepts_supported_names_and_legacy_key() {
    for key in ["scoreCurve", "ScoreCurve"] {
        for curve in [
            ScoreCurve::Standard,
            ScoreCurve::Linear,
            ScoreCurve::Logarithmic,
        ] {
            let yaml = format!("{key}: {curve:?}\n");
            let parsed: ChallengeYaml = serde_norway::from_str(&yaml).unwrap();
            assert_eq!(parsed.score_curve, Some(curve));
        }
    }
    let sparse: ChallengeYaml = serde_norway::from_str("name: sparse\n").unwrap();
    assert_eq!(sparse.score_curve, None);
}

#[test]
fn score_curve_manifest_rejects_invalid_values_and_duplicate_aliases() {
    for yaml in [
        "scoreCurve: Exponential\n",
        "scoreCurve: logarithmic\n",
        "scoreCurve: 1\n",
        "scoreCurve: [Linear]\n",
        "scoreCurve: Linear\nScoreCurve: Standard\n",
    ] {
        assert!(
            serde_norway::from_str::<ChallengeYaml>(yaml).is_err(),
            "{yaml}"
        );
    }
}

// Runs in the real PostgreSQL repository regression's migrated, isolated fixture.
pub(super) async fn assert_score_curve_sync(state: &SharedState, game_id: i32, root: &Path) {
    let dir = root.join("score-curves");
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let manifest = dir.join("challenge.yml");
    let base = "name: Curve regression\ntype: StaticAttachment\nflags: ['flag{curve}']\n";
    for curve in [
        ScoreCurve::Standard,
        ScoreCurve::Linear,
        ScoreCurve::Logarithmic,
    ] {
        let authored = format!("{base}scoreCurve: {curve:?}\n");
        tokio::fs::write(&manifest, &authored).await.unwrap();
        let created = import_with_game_lock(state, game_id, &manifest)
            .await
            .unwrap();
        assert!(created.created);
        let id = created.challenge_id;
        assert_curve(state, id, curve).await;

        // A sparse rescan preserves both authored and UI-selected values.
        tokio::fs::write(&manifest, base).await.unwrap();
        let sparse = import_with_game_lock(state, game_id, &manifest)
            .await
            .unwrap();
        assert!(!sparse.grading_update_deferred);
        assert_curve(state, id, curve).await;
        sqlx::query(r#"UPDATE "GameChallenges" SET score_curve = $2 WHERE id = $1"#)
            .bind(id)
            .bind(ScoreCurve::Logarithmic as i16)
            .execute(state.pg())
            .await
            .unwrap();
        import_with_game_lock(state, game_id, &manifest)
            .await
            .unwrap();
        assert_curve(state, id, ScoreCurve::Logarithmic).await;

        // An explicit selection (including Standard) can replace the saved curve pre-event.
        tokio::fs::write(&manifest, &authored).await.unwrap();
        import_with_game_lock(state, game_id, &manifest)
            .await
            .unwrap();
        assert_curve(state, id, curve).await;

        // Invalid values must fail before touching persisted scoring.
        tokio::fs::write(&manifest, format!("{base}scoreCurve: Unsupported\n"))
            .await
            .unwrap();
        assert!(import_with_game_lock(state, game_id, &manifest)
            .await
            .is_err());
        assert_curve(state, id, curve).await;

        // An event start locks even an unsolved challenge. Keep descriptive sync available.
        let previous_start: DateTime<Utc> =
            sqlx::query_scalar(r#"SELECT start_time_utc FROM "Games" WHERE id = $1"#)
                .bind(game_id)
                .fetch_one(state.pg())
                .await
                .unwrap();
        sqlx::query(
            r#"UPDATE "Games" SET start_time_utc = NOW() - INTERVAL '1 minute' WHERE id = $1"#,
        )
        .bind(game_id)
        .execute(state.pg())
        .await
        .unwrap();
        let other = if curve == ScoreCurve::Linear {
            ScoreCurve::Standard
        } else {
            ScoreCurve::Linear
        };
        tokio::fs::write(
            &manifest,
            format!("{base}scoreCurve: {other:?}\ndescription: refreshed\n"),
        )
        .await
        .unwrap();
        let frozen = import_with_game_lock(state, game_id, &manifest)
            .await
            .unwrap();
        assert!(frozen.grading_update_deferred);
        assert_curve(state, id, curve).await;
        let content: String =
            sqlx::query_scalar(r#"SELECT content FROM "GameChallenges" WHERE id = $1"#)
                .bind(id)
                .fetch_one(state.pg())
                .await
                .unwrap();
        assert_eq!(content, "refreshed");
        tokio::fs::write(&manifest, base).await.unwrap();
        let unchanged = import_with_game_lock(state, game_id, &manifest)
            .await
            .unwrap();
        assert!(
            !unchanged.grading_update_deferred,
            "omission must not request a curve reset"
        );
        assert_curve(state, id, curve).await;
        sqlx::query(r#"UPDATE "Games" SET start_time_utc = $2 WHERE id = $1"#)
            .bind(game_id)
            .bind(previous_start)
            .execute(state.pg())
            .await
            .unwrap();
        sqlx::query(r#"DELETE FROM "GameChallenges" WHERE id = $1"#)
            .bind(id)
            .execute(state.pg())
            .await
            .unwrap();
    }
    // Old repositories with no field retain the historical creation default.
    tokio::fs::write(&manifest, base).await.unwrap();
    let default = import_with_game_lock(state, game_id, &manifest)
        .await
        .unwrap();
    assert!(default.created);
    assert_curve(state, default.challenge_id, ScoreCurve::Standard).await;
    sqlx::query(r#"DELETE FROM "GameChallenges" WHERE id = $1"#)
        .bind(default.challenge_id)
        .execute(state.pg())
        .await
        .unwrap();
    tokio::fs::remove_dir_all(dir).await.unwrap();
}

async fn assert_curve(state: &SharedState, id: i32, expected: ScoreCurve) {
    let stored: i16 =
        sqlx::query_scalar(r#"SELECT score_curve FROM "GameChallenges" WHERE id = $1"#)
            .bind(id)
            .fetch_one(state.pg())
            .await
            .unwrap();
    assert_eq!(stored, expected as i16);
}
