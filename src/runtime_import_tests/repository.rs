//! Run the registered push queue against an owned bare repository. Exact URL
//! rewrites live only in this fixture's checkout; HTTPS transport is disabled
//! there so a broken rewrite cannot send test data or credentials to a server.
use super::*;

async fn git(root: &Path, args: &[&str]) -> String {
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "git fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

async fn enqueue(pool: &sqlx::PgPool, binding: i32, game: i32, challenge: i32, revision: i64) {
    sqlx::query(r#"INSERT INTO "RepoPushQueue"
        (binding_id, challenge_id, game_id, target_revision, lease_owner, lease_expires_at_utc)
        VALUES ($1, $2, $3, $4, $5, clock_timestamp() - interval '1 second')
        ON CONFLICT (binding_id, challenge_id) DO UPDATE SET target_revision = EXCLUDED.target_revision,
            lease_owner = EXCLUDED.lease_owner, lease_expires_at_utc = EXCLUDED.lease_expires_at_utc"#)
        .bind(binding).bind(challenge).bind(game).bind(revision).bind(Uuid::new_v4())
        .execute(pool).await.unwrap();
}

async fn drain(state: &SharedState, binding: i32) {
    let worker = *for_role(state.config.runtime_role)
        .iter()
        .find(|worker| **worker == DurableWorker::RepositoryPush)
        .unwrap();
    let (stop, signal) = watch::channel(false);
    let handle = worker.start(state.clone(), signal);
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let pending: Option<Option<String>> = sqlx::query_scalar(
                r#"SELECT last_error FROM "RepoPushQueue" WHERE binding_id = $1 LIMIT 1"#,
            )
            .bind(binding)
            .fetch_optional(state.pg())
            .await
            .unwrap();
            match pending {
                None => break,
                Some(Some(error)) => panic!("registered push worker failed: {error}"),
                Some(None) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    })
    .await;
    let _ = stop.send(true);
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .unwrap()
        .unwrap();
    result.expect("registered push worker did not finish");
}

pub(super) async fn assert_registered_push_replay(
    state: &SharedState,
    root: &Path,
    game: i32,
    challenge: i32,
) {
    // This logical URL passes the production syntax/IP validation. Git rewrites
    // it to the exact disposable local remote before opening any transport.
    let url = format!("https://1.1.1.1/rsctf-fixture/{game}.git");
    let authenticated = format!("https://x-access-token:fixture@1.1.1.1/rsctf-fixture/{game}.git");
    let binding: i32 = sqlx::query_scalar(
        r#"INSERT INTO "RepoBindings"
        (repo_url, git_ref, github_token, interval_seconds, status, created_at_utc, push_on_edit)
        VALUES ($1, 'main', 'fixture', 60, 1, clock_timestamp(), TRUE) RETURNING id"#,
    )
    .bind(&url)
    .fetch_one(state.pg())
    .await
    .unwrap();
    let checkout = root.join("repos").join(binding.to_string());
    let remote = root.join(format!("remote-{binding}.git"));
    tokio::fs::create_dir_all(checkout.join("event"))
        .await
        .unwrap();
    git(
        root,
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            remote.to_str().unwrap(),
        ],
    )
    .await;
    git(&checkout, &["init", "--initial-branch=main"]).await;
    git(&checkout, &["config", "user.name", "rsctf fixture"]).await;
    git(&checkout, &["config", "user.email", "fixture@example.test"]).await;
    tokio::fs::write(checkout.join("event/challenge.yaml"),
        b"name: Baseline\ntype: StaticAttachment\ncategory: Misc\nflags:\n  - flag{role_recovery}\n").await.unwrap();
    git(&checkout, &["add", "event/challenge.yaml"]).await;
    git(&checkout, &["commit", "-m", "fixture baseline"]).await;
    git(
        &checkout,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    )
    .await;
    git(&checkout, &["push", "-u", "origin", "main"]).await;
    let rewrite = format!("url.file://{}.insteadOf", remote.display());
    for from in [&url, &authenticated] {
        git(&checkout, &["config", "--local", "--add", &rewrite, from]).await;
    }
    git(
        &checkout,
        &["config", "--local", "protocol.https.allow", "never"],
    )
    .await;
    git(
        &checkout,
        &["config", "--local", "protocol.file.allow", "always"],
    )
    .await;
    sqlx::query(r#"UPDATE "Games" SET repo_binding_id = $2, event_manifest_path = 'event/.gzevent' WHERE id = $1"#)
        .bind(game).bind(binding).execute(state.pg()).await.unwrap();
    let revision: i64 = sqlx::query_scalar(
        r#"UPDATE "GameChallenges" SET title = 'Published from registered role',
        source_yaml_path = $2, revision = revision + 1 WHERE id = $1 RETURNING revision"#,
    )
    .bind(challenge)
    .bind(format!("binding/{binding}/event/challenge.yaml"))
    .fetch_one(state.pg())
    .await
    .unwrap();
    enqueue(state.pg(), binding, game, challenge, revision).await;
    drain(state, binding).await;
    let manifest = git(
        root,
        &[
            "--git-dir",
            remote.to_str().unwrap(),
            "show",
            "refs/heads/main:event/challenge.yaml",
        ],
    )
    .await;
    assert!(
        manifest.contains("Published from registered role"),
        "{manifest}"
    );
    assert!(manifest.contains("flag{role_recovery}"), "{manifest}");
    let head = git(
        root,
        &[
            "--git-dir",
            remote.to_str().unwrap(),
            "rev-parse",
            "refs/heads/main",
        ],
    )
    .await;

    // The push committed remotely, then its completion receipt was lost. A new
    // registered consumer must acknowledge the same revision without another
    // remote commit or a duplicate queue row.
    enqueue(state.pg(), binding, game, challenge, revision).await;
    drain(state, binding).await;
    assert_eq!(
        git(
            root,
            &[
                "--git-dir",
                remote.to_str().unwrap(),
                "rev-parse",
                "refs/heads/main"
            ]
        )
        .await,
        head
    );
    assert_eq!(
        git(
            root,
            &[
                "--git-dir",
                remote.to_str().unwrap(),
                "rev-list",
                "--count",
                "refs/heads/main"
            ]
        )
        .await,
        "2"
    );
}
