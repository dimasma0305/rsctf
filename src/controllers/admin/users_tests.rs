use super::*;

#[test]
fn internal_import_step_errors_remain_retryable() {
    let error = import_row_step::<()>(
        Err(AppError::internal(
            "postgres://operator:secret@example.test/private",
        )),
        7,
        "password_hash",
    )
    .unwrap_err();
    assert!(matches!(error, AppError::Internal(_)));
    assert!(terminal_import_row_reason(&error).is_none());
}

#[test]
fn only_deterministic_import_step_errors_become_terminal_rows() {
    let deterministic =
        import_row_step::<()>(Err(AppError::bad_request("Team is full")), 2, "provision")
            .unwrap_err();
    assert_eq!(
        terminal_import_row_reason(&deterministic).as_deref(),
        Some("Team is full")
    );
    let transient = import_row_step::<()>(
        Err(AppError::unavailable("redis endpoint details")),
        3,
        "provision",
    )
    .unwrap_err();
    assert!(terminal_import_row_reason(&transient).is_none());
}
