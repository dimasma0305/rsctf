//! Admin AI chat provider registry boundaries through the real handlers.

use super::*;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn provider_registry_protects_builtins_and_validates_custom_rules() {
    let f = fixture().await;
    assert_eq!(
        status(
            f.provider(
                "chatgpt",
                json!({ "enabled": false, "label": "x", "pattern": "https://x\\.test/a" })
            )
            .await
        ),
        400,
        "built-ins accept only the enabled switch"
    );
    assert_eq!(
        status(
            f.provider(
                "bad key",
                json!({ "enabled": true, "label": "x", "pattern": "https://x\\.test/a" })
            )
            .await
        ),
        400
    );
    for pattern in [
        r"https://.*",
        "http://poe\\.com/s/[a-z]+",
        r"https://poe\.com/s/(?=a)",
        r"^https://poe\.com/s/a$",
    ] {
        assert_eq!(
            status(
                f.provider(
                    "poe",
                    json!({ "enabled": true, "label": "Poe", "pattern": pattern })
                )
                .await
            ),
            400,
            "{pattern}"
        );
    }
    assert_eq!(
        status(f.provider("poe", json!({ "enabled": true, "label": "Poe", "pattern": r"https://poe\.com/s/[A-Za-z0-9_-]{6,64}" })).await),
        200
    );
    // Updating keeps one row; the cap counts custom providers only.
    assert_eq!(
        status(f.provider("poe", json!({ "enabled": false, "label": "Poe", "pattern": r"https://poe\.com/s/[A-Za-z0-9_-]{6,64}" })).await),
        200
    );
    for index in 1..32 {
        let key = format!("custom-{index}");
        assert_eq!(
            status(f.provider(&key, json!({ "enabled": true, "label": key, "pattern": r"https://example\.test/share/[a-z]{4}" })).await),
            200
        );
    }
    let title = error_title(
        f.provider("one-too-many", json!({ "enabled": true, "label": "x", "pattern": r"https://example\.test/share/[a-z]{4}" }))
            .await,
    )
    .await;
    assert!(title.contains("At most 32"), "{title}");

    let list = body(
        list_providers(State(f.st.clone()), AdminUser(f.admin.clone()))
            .await
            .unwrap(),
    )
    .await;
    let providers = list["providers"].as_array().unwrap();
    assert_eq!(
        providers.len(),
        crate::services::ai_chat_links::BUILTIN_PROVIDERS.len() + 32
    );
    assert!(providers
        .iter()
        .any(|p| p["key"] == "poe" && p["enabled"] == false));
    assert!(providers
        .iter()
        .filter(|p| p["builtin"] == true)
        .all(|p| !p["examples"].as_array().unwrap().is_empty()));

    assert_eq!(
        status(
            delete_provider(
                State(f.st.clone()),
                AdminUser(f.admin.clone()),
                Path("chatgpt".into())
            )
            .await
        ),
        400
    );
    assert_eq!(
        status(
            delete_provider(
                State(f.st.clone()),
                AdminUser(f.admin.clone()),
                Path("poe".into())
            )
            .await
        ),
        200
    );
    assert_eq!(
        status(
            delete_provider(
                State(f.st.clone()),
                AdminUser(f.admin.clone()),
                Path("poe".into())
            )
            .await
        ),
        404
    );
    let _ = (&f.bob, f.off_game, f.closed_game);
    f.teardown().await;
}
