use super::{MAX_PATH_CHARS, MAX_SCOPE_CHARS, RepositorySpec};
use crate::error::{
    EvalError, invalid_field, validate_identifier, validate_relative_path, validate_text,
};

pub(super) fn validate_repository(repository: &RepositorySpec) -> Result<(), EvalError> {
    if repository.path.is_some() == repository.image.is_some() {
        return Err(invalid_field(
            "repository",
            "must define exactly one of path or image",
        ));
    }
    if let Some(path) = repository.path.as_deref() {
        validate_relative_path("repository.path", path)?;
    }
    if let Some(image) = repository.image.as_deref() {
        validate_text("repository.image", image, MAX_PATH_CHARS)?;
    }
    if let Some(commit) = repository.commit.as_deref() {
        validate_identifier("repository.commit", commit, 256)?;
    }
    Ok(())
}

pub(super) fn validate_scope(scope: &[String]) -> Result<(), EvalError> {
    if scope.is_empty() {
        return Err(invalid_field(
            "write_scope",
            "must contain at least one path pattern",
        ));
    }
    for (index, pattern) in scope.iter().enumerate() {
        validate_relative_path(&format!("write_scope[{index}]"), pattern)?;
        if pattern.chars().count() > MAX_SCOPE_CHARS {
            return Err(invalid_field(
                format!("write_scope[{index}]"),
                format!("must contain at most {MAX_SCOPE_CHARS} characters"),
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_text_field(
    field: &str,
    value: &str,
    max_chars: usize,
) -> Result<(), EvalError> {
    if value.trim().is_empty() {
        return Err(invalid_field(field, "must not be blank"));
    }
    validate_text("value", value, max_chars).map_err(|error| match error {
        EvalError::InvalidField { reason, .. } => invalid_field(field, reason),
        other => other,
    })
}

pub(super) fn validate_sha256(field: &str, value: &str) -> Result<(), EvalError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid_field(
            field,
            "must be a 64-character hexadecimal SHA-256 digest",
        ));
    }
    Ok(())
}
