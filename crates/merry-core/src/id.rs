//! Validated identifier and name newtypes.

use crate::CoreError;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};
use std::{fmt, str::FromStr};
use uuid::Uuid;

const MAX_IDENTIFIER_LEN: usize = 128;
const MAX_TOOL_NAME_LEN: usize = 64;
const MAX_TOOL_CALL_ID_LEN: usize = 256;

fn validate_common_identifier(
    kind: &'static str,
    value: &str,
    max_len: usize,
) -> Result<(), CoreError> {
    if value.is_empty() {
        return Err(invalid_identifier(kind, value, "must not be empty"));
    }

    if value.trim().is_empty() {
        return Err(invalid_identifier(
            kind,
            value,
            "must not be whitespace only",
        ));
    }

    if value.trim() != value {
        return Err(invalid_identifier(
            kind,
            value,
            "must not have leading or trailing whitespace",
        ));
    }

    if value.chars().count() > max_len {
        return Err(invalid_identifier(
            kind,
            value,
            "is longer than the allowed maximum length",
        ));
    }

    if value.chars().any(char::is_control) {
        return Err(invalid_identifier(
            kind,
            value,
            "must not contain control characters",
        ));
    }

    Ok(())
}

fn validate_session_id(kind: &'static str, value: &str, max_len: usize) -> Result<(), CoreError> {
    validate_common_identifier(kind, value, max_len)?;

    if value == "." || value == ".." {
        return Err(invalid_identifier(kind, value, "must not be '.' or '..'"));
    }

    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(invalid_identifier(
            kind,
            value,
            "must contain only ASCII letters, digits, '.', '_' or '-'",
        ));
    }

    Ok(())
}

fn validate_tool_name(value: &str) -> Result<(), CoreError> {
    validate_common_identifier("ToolName", value, MAX_TOOL_NAME_LEN)?;

    let mut chars = value.chars();
    let first = chars
        .next()
        .expect("common validation rejects empty values");
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(invalid_identifier(
            "ToolName",
            value,
            "must start with an ASCII letter or '_'",
        ));
    }

    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(invalid_identifier(
            "ToolName",
            value,
            "must contain only ASCII letters, digits, '_' or '-'",
        ));
    }

    Ok(())
}

fn invalid_identifier(kind: &'static str, value: &str, reason: &'static str) -> CoreError {
    CoreError::InvalidIdentifier {
        kind,
        value: value.to_owned(),
        reason,
    }
}

macro_rules! define_id {
    ($type:ident, $kind:literal) => {
        define_id!($type, $kind, MAX_IDENTIFIER_LEN, validate_common_identifier);
    };
    ($type:ident, $kind:literal, $max_len:expr) => {
        define_id!($type, $kind, $max_len, validate_common_identifier);
    };
    ($type:ident, $kind:literal, $max_len:expr, $validator:path) => {
        #[doc = concat!("Validated ", $kind, " newtype.")]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $type(String);

        impl $type {
            /// Creates a validated identifier from a borrowed string.
            pub fn new(value: &str) -> Result<Self, CoreError> {
                $validator($kind, value, $max_len)?;
                Ok(Self(value.to_owned()))
            }

            /// Borrows the identifier as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $type {
            type Err = CoreError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<&str> for $type {
            type Error = CoreError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $type {
            type Error = CoreError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                $validator($kind, &value, $max_len)?;
                Ok(Self(value))
            }
        }

        impl<'de> Deserialize<'de> for $type {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::try_from(value).map_err(de::Error::custom)
            }
        }
    };
}

define_id!(
    SessionId,
    "SessionId",
    MAX_IDENTIFIER_LEN,
    validate_session_id
);
define_id!(ArtifactId, "ArtifactId");
define_id!(SkillId, "SkillId");
define_id!(SubagentId, "SubagentId");
define_id!(SubagentTaskId, "SubagentTaskId");
define_id!(ProviderName, "ProviderName");
define_id!(ToolCallBatchId, "ToolCallBatchId");
define_id!(ToolCallId, "ToolCallId", MAX_TOOL_CALL_ID_LEN);

impl SessionId {
    /// Generates a random filesystem-safe session id.
    #[must_use]
    pub fn random() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

/// Provider-portable tool name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct ToolName(String);

impl ToolName {
    /// Creates a validated provider-portable tool name.
    pub fn new(value: &str) -> Result<Self, CoreError> {
        validate_tool_name(value)?;
        Ok(Self(value.to_owned()))
    }

    /// Borrows the tool name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ToolName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for ToolName {
    type Err = CoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for ToolName {
    type Error = CoreError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for ToolName {
    type Error = CoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        validate_tool_name(&value)?;
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for ToolName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_from(value).map_err(de::Error::custom)
    }
}
