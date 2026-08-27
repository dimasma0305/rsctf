//! One-time publication of immutable final scoreboard variants.

use super::*;

struct FinalVariant {
    modes: (bool, bool),
    values: Vec<(String, bytes::Bytes)>,
}

fn final_variant_keys(game_id: i32, is_monitor: bool, has_ad: bool, has_koth: bool) -> Vec<String> {
    let mut keys = Vec::with_capacity(6);
    if is_monitor {
        keys.push(format!("_ScoreBoardWireV2_{game_id}"));
        keys.push(format!("_CombinedScoreBoardByChallenge_{game_id}"));
        if has_ad {
            keys.push(format!("_AdScoreBoard_{game_id}"));
            keys.push(format!("_AdScoreBoard_{game_id}:stale"));
        }
        if has_koth {
            keys.push(format!("_KothScoreBoardWireV2_{game_id}"));
            keys.push(format!("_KothTimeline_{game_id}"));
        }
    } else {
        keys.push(format!("_ScoreBoardWireV2Frozen_{game_id}"));
        keys.push(format!("_CombinedScoreBoardByChallengeFrozen_{game_id}"));
        if has_ad {
            keys.push(format!("_AdScoreBoardFrozen_{game_id}"));
            keys.push(format!("_AdScoreBoardFrozen_{game_id}:stale"));
        }
        if has_koth {
            keys.push(format!("_KothScoreBoardWireV2Frozen_{game_id}"));
            keys.push(format!("_KothTimelineFrozen_{game_id}"));
        }
    }
    keys
}

async fn build_final_variant(
    st: &SharedState,
    game: &game::Model,
    is_monitor: bool,
) -> AppResult<Option<FinalVariant>> {
    // Deliberately bypass every render cache. A fill that began before the
    // closeout barrier may briefly exist while it loses its revision fence; a
    // finalizer must never consume that value and declare it immutable.
    let jeopardy = build_scoreboard(st, game, is_monitor).await?;
    let counts = ModeChallengeCounts::from_board(&jeopardy);
    let ad_future = async {
        if counts.attack_defense > 0 {
            crate::services::ad::scoring::build_ad_scoreboard(
                st.pg(),
                game.id,
                is_monitor,
                Utc::now(),
            )
            .await
            .map(Some)
        } else {
            Ok(None)
        }
    };
    let koth_future = async {
        if counts.koth > 0 {
            koth::build_koth_scoreboard(st, game, is_monitor, Utc::now())
                .await
                .map(Some)
        } else {
            Ok(None)
        }
    };
    let (ad, koth_board, divisions) =
        tokio::try_join!(ad_future, koth_future, load_division_access(st, game.id))?;
    if ad.as_ref().is_some_and(|board| !board.fully_settled)
        || koth_board
            .as_ref()
            .is_some_and(|board| !board.fully_settled)
    {
        return Ok(None);
    }

    let mut values = Vec::with_capacity(5);
    let standard_key = if is_monitor {
        format!("_ScoreBoardWireV2_{}", game.id)
    } else {
        format!("_ScoreBoardWireV2Frozen_{}", game.id)
    };
    let standard_raw = bytes::Bytes::from(
        serde_json::to_vec(&jeopardy).map_err(|error| AppError::internal(error.to_string()))?,
    );
    let standard = super::scoreboard_encoding::build_stable_bundle(
        standard_raw,
        standard_key.clone(),
        b"\"updateTimeUtc\":",
    )
    .await?;
    if !standard.cacheable {
        return Err(AppError::internal(
            "final standard scoreboard exceeds the cache value limit",
        ));
    }
    values.push((standard_key, standard.bytes));

    if let Some(board) = ad.as_ref() {
        let raw = bytes::Bytes::from(
            serde_json::to_vec(board).map_err(|error| AppError::internal(error.to_string()))?,
        );
        let built = super::scoreboard_encoding::build_bundle(raw).await?;
        if !built.cacheable {
            return Err(AppError::internal(
                "final A&D scoreboard exceeds the cache value limit",
            ));
        }
        let key = if is_monitor {
            format!("_AdScoreBoard_{}", game.id)
        } else {
            format!("_AdScoreBoardFrozen_{}", game.id)
        };
        values.push((key.clone(), built.bytes.clone()));
        values.push((format!("{key}:stale"), built.bytes));
    }
    if let Some(board) = koth_board.as_ref() {
        let key = if is_monitor {
            format!("_KothScoreBoardWireV2_{}", game.id)
        } else {
            format!("_KothScoreBoardWireV2Frozen_{}", game.id)
        };
        let raw = bytes::Bytes::from(
            serde_json::to_vec(board).map_err(|error| AppError::internal(error.to_string()))?,
        );
        let built =
            super::scoreboard_encoding::build_stable_bundle(raw, key.clone(), b"\"generatedAt\":")
                .await?;
        if !built.cacheable {
            return Err(AppError::internal(
                "final KotH scoreboard exceeds the cache value limit",
            ));
        }
        values.push((key, built.bytes));
    }

    let combined = combine_scoreboards(game, jeopardy, ad, koth_board, divisions, counts);
    let built = encode_combined_scoreboard(&combined).await?;
    if !built.cacheable {
        return Err(AppError::internal(
            "final combined scoreboard exceeds the cache value limit",
        ));
    }
    values.push((combined_cache_key(game.id, is_monitor), built.bytes));
    Ok(Some(FinalVariant {
        modes: (counts.attack_defense > 0, counts.koth > 0),
        values,
    }))
}

/// Build and publish all authorized final scoreboard variants after durable
/// closeout evidence has settled. Returns `false` while an A&D/KotH component
/// is still provisional so the leased maintenance job can retry later.
pub(crate) async fn materialize_final_scoreboards(
    st: &SharedState,
    game: &game::Model,
) -> AppResult<bool> {
    let now = Utc::now();
    if now < game.end_time_utc {
        return Ok(false);
    }
    if game.practice_mode {
        return Err(AppError::internal(
            "practice scoreboards remain mutable after the scheduled end",
        ));
    }
    if st.cache.backend_health().await == crate::services::cache::CacheBackendHealth::Unavailable {
        return Err(AppError::internal(
            "final scoreboard cache backend is unavailable",
        ));
    }

    'attempt: for _ in 0..2 {
        let Some((snapshot, revision)) =
            super::load_scoreboard_game_revision(st, game.id, true).await?
        else {
            return Err(AppError::not_found("Game not found"));
        };
        if snapshot.end_time_utc != game.end_time_utc
            || snapshot.practice_mode
            || Utc::now() < snapshot.end_time_utc
        {
            return Ok(false);
        }
        let game = &snapshot;
        let mut expected_keys = Vec::with_capacity(12);
        let mut expected_modes = None;
        for is_monitor in [true, false] {
            if game.hidden && !is_monitor {
                continue;
            }
            let Some(variant) = build_final_variant(st, game, is_monitor).await? else {
                return Ok(false);
            };
            if expected_modes.is_some_and(|expected| expected != variant.modes) {
                return Err(AppError::internal(
                    "final scoreboard variants disagree on active formats",
                ));
            }
            for (key, value) in variant.values {
                match super::publish_scoreboard_render(
                    st,
                    game.id,
                    is_monitor,
                    &revision,
                    &key,
                    &value,
                    crate::controllers::game::FINAL_SCOREBOARD_CACHE_TTL,
                )
                .await?
                {
                    super::ScoreboardPublish::Published => {}
                    super::ScoreboardPublish::RevisionChanged => {
                        super::invalidate_scoreboard_render_version(st, game.id).await?;
                        continue 'attempt;
                    }
                    super::ScoreboardPublish::GameMissing => {
                        return Err(AppError::not_found("Game not found"));
                    }
                }
            }
            expected_modes = Some(variant.modes);
            expected_keys.extend(final_variant_keys(
                game.id,
                is_monitor,
                variant.modes.0,
                variant.modes.1,
            ));
        }

        if expected_modes.is_some_and(|modes| modes.1)
            && !koth::materialize_final_timelines(st, game).await?
        {
            return Ok(false);
        }
        if super::scoreboard_render_revision(st, game.id, true)
            .await?
            .as_deref()
            != Some(revision.as_str())
        {
            super::invalidate_scoreboard_render_version(st, game.id).await?;
            continue;
        }
        for key in &expected_keys {
            if st.cache.get_authoritative(key).await.is_none() {
                return Err(AppError::internal(format!(
                    "final scoreboard cache publication failed for {key}"
                )));
            }
        }
        if st.cache.backend_health().await
            == crate::services::cache::CacheBackendHealth::Unavailable
        {
            return Err(AppError::internal(
                "final scoreboard cache backend became unavailable",
            ));
        }
        if super::scoreboard_render_revision(st, game.id, true)
            .await?
            .as_deref()
            == Some(revision.as_str())
        {
            return Ok(true);
        }
        super::invalidate_scoreboard_render_version(st, game.id).await?;
    }
    Err(AppError::internal(
        "scoreboard changed during both final publication attempts",
    ))
}
