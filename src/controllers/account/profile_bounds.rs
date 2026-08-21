use sea_orm::EntityTrait;
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::models::data::user;
use crate::utils::error::{AppError, AppResult};

const MAX_PROFILE_BIO_CHARS: usize = 4_096;
const MAX_PROFILE_PHONE_CHARS: usize = 64;
const MAX_PROFILE_REAL_NAME_CHARS: usize = 256;
const MAX_PROFILE_STD_NUMBER_CHARS: usize = 128;

pub(crate) fn validate_profile_fields(
    bio: Option<&str>,
    phone: Option<&str>,
    real_name: Option<&str>,
    std_number: Option<&str>,
) -> AppResult<()> {
    for (label, value, maximum) in [
        ("Bio", bio, MAX_PROFILE_BIO_CHARS),
        ("Phone number", phone, MAX_PROFILE_PHONE_CHARS),
        ("Real name", real_name, MAX_PROFILE_REAL_NAME_CHARS),
        ("Student number", std_number, MAX_PROFILE_STD_NUMBER_CHARS),
    ] {
        if value.is_some_and(|value| value.chars().count() > maximum) {
            return Err(AppError::bad_request(format!(
                "{label} cannot exceed {maximum} characters"
            )));
        }
    }
    Ok(())
}

pub(super) async fn load_user(st: &SharedState, id: Uuid) -> AppResult<user::Model> {
    user::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("User not found"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_text_is_bounded_by_characters() {
        assert!(validate_profile_fields(Some(&"x".repeat(4_096)), None, None, None).is_ok());
        assert!(validate_profile_fields(Some(&"x".repeat(4_097)), None, None, None).is_err());
        assert!(validate_profile_fields(None, Some(&"x".repeat(65)), None, None).is_err());
    }
}
