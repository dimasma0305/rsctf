use crate::utils::error::{AppError, AppResult};

use super::MAX_PASSWORD_BYTES;

/// Mirror of RSCTF's ASP.NET Identity password policy (IdentityExtension:
/// `RequireNonAlphanumeric = false`, `RequireDigit = true`, `RequireUppercase =
/// true`, `RequireLowercase = true`, `RequiredLength = 6`). RSCTF runs this inside
/// `UserManager.CreateAsync` / `ChangePasswordAsync` / `ResetPasswordAsync` and
/// surfaces the first failing validator's description through `HandleIdentityError`
/// as a 400. We reproduce Identity's `PasswordValidator` check order (length, then
/// digit, lowercase, uppercase) and its default `IdentityError` descriptions so the
/// 400 body matches RSCTF's.
pub(in crate::controllers) fn validate_password(pw: &str) -> AppResult<()> {
    if pw.len() > MAX_PASSWORD_BYTES {
        return Err(AppError::bad_request(format!(
            "Passwords cannot exceed {MAX_PASSWORD_BYTES} bytes."
        )));
    }
    if pw.chars().count() < 6 {
        return Err(AppError::bad_request(
            "Passwords must be at least 6 characters.",
        ));
    }
    if !pw.chars().any(|c| c.is_ascii_digit()) {
        return Err(AppError::bad_request(
            "Passwords must have at least one digit ('0'-'9').",
        ));
    }
    if !pw.chars().any(char::is_lowercase) {
        return Err(AppError::bad_request(
            "Passwords must have at least one lowercase ('a'-'z').",
        ));
    }
    if !pw.chars().any(char::is_uppercase) {
        return Err(AppError::bad_request(
            "Passwords must have at least one uppercase ('A'-'Z').",
        ));
    }
    Ok(())
}
