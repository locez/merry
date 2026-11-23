#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModelBindingTarget {
    ContextCompaction,
    ApprovalReview,
    SubagentDefault,
    SubagentRole(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReasoningBindingTarget {
    ActiveProvider,
    Compaction,
    ApprovalReview,
    SubagentDefault,
    SubagentRole(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModelPickerTarget {
    ActiveProvider,
    QuickSwitch,
    SettingsActiveModel,
    ProviderForm,
    RoleBinding(ModelBindingTarget),
    SettingsReasoning(ReasoningBindingTarget),
}
