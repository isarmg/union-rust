use crate::error::{AppError, AppResult};

/// Identity text is required, bounded, and free of control characters.
pub(super) fn validate_text(field: &str, value: &str, max: usize) -> AppResult<()> {
    if value.trim().is_empty() {
        return Err(AppError::BadRequest(format!("invalid {field}")));
    }
    validate_bounded_text(field, value, max)
}

/// Descriptive text may be absent or empty, but remains bounded and free of controls.
pub(super) fn validate_optional_text(
    field: &str,
    value: Option<&str>,
    max: usize,
) -> AppResult<()> {
    match value {
        Some(value) => validate_bounded_text(field, value, max),
        None => Ok(()),
    }
}

fn validate_bounded_text(field: &str, value: &str, max: usize) -> AppResult<()> {
    if value.len() > max {
        return Err(AppError::BadRequest(format!(
            "{field} exceeds {max} bytes (got {})",
            value.len()
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(AppError::BadRequest(format!(
            "{field} must not contain control characters"
        )));
    }
    Ok(())
}

pub(super) fn validate_percent(field: &str, value: f64) -> AppResult<()> {
    if !value.is_finite() || !(0.0..=100.0).contains(&value) {
        return Err(AppError::BadRequest(format!("invalid {field}")));
    }
    Ok(())
}

pub(super) fn validate_nonnegative_rate(field: &str, value: f64) -> AppResult<()> {
    if !value.is_finite() || value < 0.0 {
        return Err(AppError::BadRequest(format!("invalid {field}")));
    }
    Ok(())
}

pub(super) fn validate_sha256_hex(field: &str, value: &str) -> AppResult<()> {
    if value.len() != 64
        || !value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err(AppError::BadRequest(format!(
            "{field} must be a lowercase SHA-256 hex digest"
        )));
    }
    Ok(())
}

pub(super) fn validate_canonical_uuid(field: &str, value: &str) -> AppResult<()> {
    let parsed = uuid::Uuid::parse_str(value).map_err(|_| {
        AppError::BadRequest(format!(
            "{field} must be a canonical lowercase, hyphenated UUID"
        ))
    })?;
    if parsed.to_string() != value {
        return Err(AppError::BadRequest(format!(
            "{field} must be a canonical lowercase, hyphenated UUID"
        )));
    }
    Ok(())
}
