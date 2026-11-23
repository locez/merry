use super::{CodingAutoFallback, CodingPermissionMode};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Who reviews permission requests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub(crate) enum PermissionMode {
    /// The approval-review model decides first.
    #[default]
    Auto,
    /// The person at the terminal decides.
    Ask,
    /// Configured actions run without a review round.
    AlwaysApprove,
    /// Refuse every permission request. The working model receives the denial.
    Deny,
}

/// What happens after `auto` denies an action or cannot decide.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub(crate) enum AutoFallback {
    /// Ask the person at the terminal.
    #[default]
    Ask,
    /// Return the denial to the working model. That model decides when the turn ends.
    Deny,
}

impl PermissionMode {
    pub(crate) const fn coding_mode(self) -> CodingPermissionMode {
        match self {
            Self::Auto => CodingPermissionMode::Auto,
            Self::Ask => CodingPermissionMode::Ask,
            Self::AlwaysApprove => CodingPermissionMode::AlwaysApprove,
            Self::Deny => CodingPermissionMode::Deny,
        }
    }
}

impl AutoFallback {
    pub(crate) const fn coding_fallback(self) -> CodingAutoFallback {
        match self {
            Self::Ask => CodingAutoFallback::Ask,
            Self::Deny => CodingAutoFallback::Deny,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AutoFallback, PermissionMode};
    use clap::ValueEnum;

    #[test]
    fn value_names_match_config_spellings() {
        for mode in PermissionMode::value_variants() {
            let name = mode
                .to_possible_value()
                .expect("every mode is a flag value")
                .get_name()
                .to_owned();
            let parsed: PermissionMode =
                serde_json::from_value(serde_json::Value::String(name.clone()))
                    .unwrap_or_else(|error| panic!("{name} should deserialize: {error}"));
            assert_eq!(parsed, *mode);
        }
        for fallback in AutoFallback::value_variants() {
            let name = fallback
                .to_possible_value()
                .expect("every fallback is a flag value")
                .get_name()
                .to_owned();
            let parsed: AutoFallback =
                serde_json::from_value(serde_json::Value::String(name.clone()))
                    .unwrap_or_else(|error| panic!("{name} should deserialize: {error}"));
            assert_eq!(parsed, *fallback);
        }
        assert_eq!(PermissionMode::default(), PermissionMode::Auto);
        assert_eq!(AutoFallback::default(), AutoFallback::Ask);
    }
}
