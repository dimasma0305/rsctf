use super::*;

#[tokio::test]
async fn failed_destroy_never_reaches_exercise_owner_cleanup() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://rsctf:rsctf@127.0.0.1:1/rsctf")
        .unwrap();
    let error = destroy_owned_exercise_container_with(
        &pool,
        Some(7),
        uuid::Uuid::nil(),
        "runtime-7",
        None,
        async { Err(AppError::internal("injected destroy failure")) },
    )
    .await
    .unwrap_err();

    assert_eq!(error.to_string(), "injected destroy failure");
}

#[test]
fn eligible_flags_are_scoped_to_current_dynamic_or_static_rows() {
    assert!(ELIGIBLE_EXERCISE_FLAG_SQL.contains("exercise_id = $1"));
    assert!(ELIGIBLE_EXERCISE_FLAG_SQL.contains("flag.flag = $3"));
    assert!(ELIGIBLE_EXERCISE_FLAG_SQL.contains("flag.id = $2 AND flag.is_occupied = TRUE"));
    assert!(ELIGIBLE_EXERCISE_FLAG_SQL.contains("OR flag.is_occupied = FALSE"));
}

#[test]
fn exercise_answer_uses_the_normal_flag_byte_limit() {
    let maximum = crate::utils::flag_policy::NORMAL_FLAG_MAX_BYTES;
    assert!(validated_exercise_answer(&"a".repeat(maximum)).is_ok());
    assert!(matches!(
        validated_exercise_answer(&"a".repeat(maximum + 1)),
        Err(AppError::BadRequest(message))
            if message == format!(
                "Flag is {} UTF-8 bytes; the maximum is {maximum} bytes",
                maximum + 1
            )
    ));
    assert!(validated_exercise_answer("  \t").is_err());
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn exercise_flags_reject_other_owners_and_cleanup_stale_instances() {
    use sqlx::postgres::PgPoolOptions;

    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("exercise_flags_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = crate::migrations::test_pg_connect_options(&database_url)
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TABLE "FlagContexts" (
          id SERIAL PRIMARY KEY, flag TEXT NOT NULL,
          is_occupied BOOLEAN NOT NULL, exercise_id INTEGER
        );
        CREATE TABLE "ExerciseInstances" (
          id INTEGER PRIMARY KEY, container_id UUID,
          is_loaded BOOLEAN NOT NULL, flag_id INTEGER
        );
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let own_flag_id = sqlx::query_scalar::<_, i32>(
        r#"INSERT INTO "FlagContexts" (flag, is_occupied, exercise_id)
           VALUES ('flag{own}', TRUE, 9) RETURNING id"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "FlagContexts" (flag, is_occupied, exercise_id)
           VALUES ('flag{other}', TRUE, 9), ('flag{static}', FALSE, 9)"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut connection = pool.acquire().await.unwrap();
    assert!(
        eligible_exercise_flag(&mut connection, 9, Some(own_flag_id), "flag{own}")
            .await
            .unwrap()
    );
    assert!(
        eligible_exercise_flag(&mut connection, 9, Some(own_flag_id), "flag{static}")
            .await
            .unwrap()
    );
    assert!(
        !eligible_exercise_flag(&mut connection, 9, Some(own_flag_id), "flag{other}")
            .await
            .unwrap()
    );
    assert!(
        eligible_exercise_flag(&mut connection, 9, None, "flag{static}")
            .await
            .unwrap()
    );
    assert!(
        !eligible_exercise_flag(&mut connection, 9, None, "flag{own}")
            .await
            .unwrap()
    );
    drop(connection);

    let container_id = uuid::Uuid::new_v4();
    sqlx::query(r#"INSERT INTO "ExerciseInstances" VALUES (41, $1, TRUE, $2)"#)
        .bind(container_id)
        .bind(own_flag_id)
        .execute(&pool)
        .await
        .unwrap();
    clear_exercise_container_owner(&pool, Some(41), container_id, None, Some(own_flag_id))
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_as::<_, (Option<uuid::Uuid>, bool, Option<i32>)>(
            r#"SELECT container_id, is_loaded, flag_id
                 FROM "ExerciseInstances" WHERE id = 41"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        (None, false, None)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "FlagContexts" WHERE id = $1"#)
            .bind(own_flag_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
