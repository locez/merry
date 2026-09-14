use super::CodingApprovalPolicy;
use clap::ValueEnum;
use serde::Deserialize;

/// Who reviews permission requests, as written for `--approval-policy` and
/// `[cli] approval_policy`. Each value names the reviewer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub(crate) enum ApprovalPolicy {
    /// Follow the sandbox mode: model under --with-sandbox, model then human
    /// under --inner-sandbox, human under --no-sandbox
    #[default]
    Auto,
    /// The approval-review model decides; no human fallback
    Model,
    /// You decide in the TUI dialog or on the run prompt
    Human,
    /// The model decides; you are asked only when the model cannot
    ModelThenHuman,
    /// Nobody reviews: skip model and human review for configured actions
    Trusted,
    /// Reject every permission request without asking
    #[serde(rename = "none")]
    #[value(name = "none")]
    DenyAll,
}

impl ApprovalPolicy {
    /// The value as written on the command line and in `config.toml`.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Model => "model",
            Self::Human => "human",
            Self::ModelThenHuman => "model-then-human",
            Self::Trusted => "trusted",
            Self::DenyAll => "none",
        }
    }
}

impl From<ApprovalPolicy> for CodingApprovalPolicy {
    fn from(policy: ApprovalPolicy) -> Self {
        match policy {
            ApprovalPolicy::Auto => Self::Auto,
            ApprovalPolicy::Model => Self::Model,
            ApprovalPolicy::Human => Self::Human,
            ApprovalPolicy::ModelThenHuman => Self::ModelThenHuman,
            ApprovalPolicy::Trusted => Self::Trusted,
            ApprovalPolicy::DenyAll => Self::DenyAll,
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
        assert_eq!(ApprovalPolicy::DenyAll.name(), "none");
        assert_eq!(ApprovalPolicy::default(), ApprovalPolicy::Auto);
    }
}
