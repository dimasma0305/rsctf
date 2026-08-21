use super::*;

#[test]
fn email_confirmation_precedes_active_on_register_for_non_bootstrap_accounts() {
    assert_eq!(
        registration_disposition(false, false, false),
        (false, RegisterStatus::AdminConfirmationRequired)
    );
    assert_eq!(
        registration_disposition(false, false, true),
        (false, RegisterStatus::EmailConfirmationRequired)
    );
    assert_eq!(
        registration_disposition(false, true, false),
        (true, RegisterStatus::LoggedIn)
    );
    assert_eq!(
        registration_disposition(false, true, true),
        (false, RegisterStatus::EmailConfirmationRequired)
    );
    assert_eq!(
        registration_disposition(true, false, true),
        (true, RegisterStatus::LoggedIn)
    );
}
