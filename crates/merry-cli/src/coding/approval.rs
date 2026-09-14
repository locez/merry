use super::CodingApprovalPolicy;
use clap::ValueEnum;
use serde::Deserialize;

/// Who reviews permission requests, as written for `--approval-policy` and
/// `[cli] approval_policy`. Each value names the reviewer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub(crate) enum ApprovalPolicy {
    /// No approval needed: configured actions run without model or human review
    #[serde(rename = "none")]
    #[value(name = "none")]
    NoApproval,
    /// Reject every permission request without asking
    Deny,
    /// The approval-review model decides; no human fallback
    ModelOnly,
    /// The model reviews first; you decide when it denies or cannot decide
    #[default]
    ModelThenHuman,
    /// You decide in the TUI dialog or on the run prompt
    HumanOnly,
}

impl ApprovalPolicy {
    /// The value as written on the command line and in `config.toml`.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::NoApproval => "none",
            Self::Deny => "deny",
            Self::ModelOnly => "model_only",
            Self::ModelThenHuman => "model_then_human",
            Self::HumanOnly => "human_only",
        }
    }
}

impl From<ApprovalPolicy> for CodingApprovalPolicy {
    fn from(policy: ApprovalPolicy) -> Self {
        match policy {
            ApprovalPolicy::NoApproval => Self::NoApproval,
            ApprovalPolicy::Deny => Self::Deny,
            ApprovalPolicy::ModelOnly => Self::ModelOnly,
            ApprovalPolicy::ModelThenHuman => Self::ModelThenHuman,
            ApprovalPolicy::HumanOnly => Self::HumanOnly,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ApprovalPolicy;
    use clap::ValueEnum;

    #[test]
    fn value_names_match_config_and_flag_spellings() {
        for policy in ApprovalPolicy::value_variants() {
            let flag_name = policy
                .to_possible_value()
                .expect("every policy is a flag value")
                .get_name()
                .to_owned();
            assert_eq!(flag_name, policy.name());
            let parsed: ApprovalPolicy =
                serde_json::from_value(serde_json::Value::String(flag_name.clone()))
                    .unwrap_or_else(|error| panic!("{flag_name} should deserialize: {error}"));
            assert_eq!(parsed, *policy);
        }
        assert_eq!(ApprovalPolicy::NoApproval.name(), "none");
        assert_eq!(ApprovalPolicy::default(), ApprovalPolicy::ModelThenHuman);
    }
}
