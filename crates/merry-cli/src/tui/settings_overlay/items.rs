//! Settings categories, items, labels, descriptions, and behavior predicates.

use crate::{config::HeaderItemKind, tui::keymap::KeyAction};

/// Top-level category in the grouped Settings Workbench.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SettingsCategory {
    #[default]
    Appearance,
    Interaction,
    Models,
    Context,
    Agents,
    Permissions,
    Shortcuts,
}

impl SettingsCategory {
    pub(crate) const ALL: [Self; 7] = [
        Self::Appearance,
        Self::Interaction,
        Self::Models,
        Self::Context,
        Self::Agents,
        Self::Permissions,
        Self::Shortcuts,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Interaction => "Interaction",
            Self::Models => "Models",
            Self::Context => "Context",
            Self::Agents => "Agents",
            Self::Permissions => "Permissions",
            Self::Shortcuts => "Shortcuts",
        }
    }

    pub(crate) const fn summary(self) -> &'static str {
        match self {
            Self::Appearance => "Theme, header, layout",
            Self::Interaction => "Scroll & output preview",
            Self::Models => "Provider, model, effort",
            Self::Context => "Window & auto compaction",
            Self::Agents => "Subagents & parallel tools",
            Self::Permissions => "Sandbox, access & review",
            Self::Shortcuts => "Keyboard shortcuts",
        }
    }

    pub(crate) fn items(self) -> &'static [SettingItem] {
        match self {
            Self::Appearance => &[
                SettingItem::CodeTheme,
                SettingItem::UiTheme,
                SettingItem::TransparentBackground,
                SettingItem::Animations,
                SettingItem::BrandMark,
                SettingItem::MaxReadingWidth,
                SettingItem::TextSizing,
                SettingItem::HeaderItem(HeaderItemKind::Workspace),
                SettingItem::HeaderItem(HeaderItemKind::Model),
                SettingItem::HeaderItem(HeaderItemKind::Context),
                SettingItem::HeaderItem(HeaderItemKind::Cache),
                SettingItem::HeaderItem(HeaderItemKind::Rate),
                SettingItem::HeaderItem(HeaderItemKind::Tokens),
            ],
            Self::Interaction => &[
                SettingItem::MouseScrollLines,
                SettingItem::ShowSuccessfulCommandOutput,
            ],
            Self::Models => &[
                SettingItem::ActiveProvider,
                SettingItem::ActiveModel,
                SettingItem::ReasoningEffort,
                SettingItem::ServiceTier,
                SettingItem::CompactionProvider,
                SettingItem::CompactionModel,
                SettingItem::CompactionReasoning,
                SettingItem::ApprovalReviewProvider,
                SettingItem::ApprovalReviewModel,
                SettingItem::ApprovalReviewReasoning,
                SettingItem::ManageProviders,
            ],
            Self::Context => &[
                SettingItem::ContextWindow,
                SettingItem::AutoCompaction,
                SettingItem::ContextStrategy,
                SettingItem::RetainedModelTurns,
            ],
            Self::Agents => &[
                SettingItem::Subagents,
                SettingItem::MaxThreads,
                SettingItem::SubagentLimitBehavior,
                SettingItem::MaxParallelToolCalls,
                SettingItem::SubagentDefaultProvider,
                SettingItem::SubagentDefaultModel,
                SettingItem::SubagentDefaultReasoning,
                SettingItem::SubagentRoleSelector,
                SettingItem::SubagentRoleEnabled,
                SettingItem::SubagentRoleProvider,
                SettingItem::SubagentRoleModel,
                SettingItem::SubagentRoleReasoning,
                SettingItem::SubagentRoleMaxTurns,
                SettingItem::SubagentRoleTimeout,
            ],
            Self::Permissions => &[
                SettingItem::PermissionSandbox,
                SettingItem::PermissionMode,
                SettingItem::PermissionAutoFallback,
                SettingItem::PermissionNetwork,
                SettingItem::PermissionSshAgent,
                SettingItem::PermissionGpgAgent,
                SettingItem::PermissionDbus,
            ],
            Self::Shortcuts => &[
                SettingItem::Shortcut(KeyAction::Submit),
                SettingItem::Shortcut(KeyAction::InsertNewline),
                SettingItem::Shortcut(KeyAction::PasteImage),
                SettingItem::Shortcut(KeyAction::TogglePlan),
                SettingItem::Shortcut(KeyAction::OpenCommandDetails),
                SettingItem::Shortcut(KeyAction::FollowLatest),
                #[cfg(feature = "web")]
                SettingItem::Shortcut(KeyAction::OpenSessionInBrowser),
                SettingItem::Shortcut(KeyAction::ReviewPreviousUserInput),
                SettingItem::Shortcut(KeyAction::Interrupt),
                SettingItem::Shortcut(KeyAction::CancelInputOrQuit),
                SettingItem::Shortcut(KeyAction::Quit),
                SettingItem::Shortcut(KeyAction::ScrollUp),
                SettingItem::Shortcut(KeyAction::ScrollDown),
                SettingItem::Shortcut(KeyAction::HistoryPrevious),
                SettingItem::Shortcut(KeyAction::HistoryNext),
                SettingItem::Shortcut(KeyAction::HistorySearch),
                SettingItem::Shortcut(KeyAction::ToggleComment),
            ],
        }
    }

    pub(crate) fn step(self, delta: isize) -> Self {
        let index = Self::ALL
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0);
        let len = Self::ALL.len();
        let next = if delta < 0 {
            (index + len - 1) % len
        } else {
            (index + 1) % len
        };
        Self::ALL[next]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingItem {
    CodeTheme,
    UiTheme,
    TransparentBackground,
    Animations,
    BrandMark,
    MaxReadingWidth,
    TextSizing,
    HeaderItem(HeaderItemKind),
    MouseScrollLines,
    ShowSuccessfulCommandOutput,
    ActiveProvider,
    ActiveModel,
    ReasoningEffort,
    ServiceTier,
    CompactionProvider,
    CompactionModel,
    CompactionReasoning,
    ApprovalReviewProvider,
    ApprovalReviewModel,
    ApprovalReviewReasoning,
    ManageProviders,
    ContextWindow,
    AutoCompaction,
    ContextStrategy,
    RetainedModelTurns,
    Subagents,
    MaxThreads,
    SubagentLimitBehavior,
    MaxParallelToolCalls,
    SubagentDefaultProvider,
    SubagentDefaultModel,
    SubagentDefaultReasoning,
    SubagentRoleSelector,
    SubagentRoleEnabled,
    SubagentRoleProvider,
    SubagentRoleModel,
    SubagentRoleReasoning,
    SubagentRoleMaxTurns,
    SubagentRoleTimeout,
    PermissionSandbox,
    PermissionMode,
    PermissionAutoFallback,
    PermissionNetwork,
    PermissionSshAgent,
    PermissionGpgAgent,
    PermissionDbus,
    Shortcut(KeyAction),
}

impl SettingItem {
    pub(crate) const fn category(self) -> SettingsCategory {
        match self {
            Self::CodeTheme
            | Self::UiTheme
            | Self::TransparentBackground
            | Self::Animations
            | Self::BrandMark
            | Self::MaxReadingWidth
            | Self::TextSizing
            | Self::HeaderItem(_) => SettingsCategory::Appearance,
            Self::MouseScrollLines | Self::ShowSuccessfulCommandOutput => {
                SettingsCategory::Interaction
            }
            Self::ActiveProvider
            | Self::ActiveModel
            | Self::ReasoningEffort
            | Self::ServiceTier
            | Self::CompactionProvider
            | Self::CompactionModel
            | Self::CompactionReasoning
            | Self::ApprovalReviewProvider
            | Self::ApprovalReviewModel
            | Self::ApprovalReviewReasoning
            | Self::ManageProviders => SettingsCategory::Models,
            Self::ContextWindow
            | Self::AutoCompaction
            | Self::ContextStrategy
            | Self::RetainedModelTurns => SettingsCategory::Context,
            Self::Subagents
            | Self::MaxThreads
            | Self::SubagentLimitBehavior
            | Self::MaxParallelToolCalls
            | Self::SubagentDefaultProvider
            | Self::SubagentDefaultModel
            | Self::SubagentDefaultReasoning
            | Self::SubagentRoleSelector
            | Self::SubagentRoleEnabled
            | Self::SubagentRoleProvider
            | Self::SubagentRoleModel
            | Self::SubagentRoleReasoning
            | Self::SubagentRoleMaxTurns
            | Self::SubagentRoleTimeout => SettingsCategory::Agents,
            Self::PermissionSandbox
            | Self::PermissionMode
            | Self::PermissionAutoFallback
            | Self::PermissionNetwork
            | Self::PermissionSshAgent
            | Self::PermissionGpgAgent
            | Self::PermissionDbus => SettingsCategory::Permissions,
            Self::Shortcut(_) => SettingsCategory::Shortcuts,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::CodeTheme => "Code theme",
            Self::UiTheme => "UI theme",
            Self::TransparentBackground => "Transparent bg",
            Self::Animations => "Animations",
            Self::BrandMark => "Brand mark",
            Self::MaxReadingWidth => "Max reading width",
            Self::TextSizing => "Text sizing",
            Self::HeaderItem(HeaderItemKind::Workspace) => "Header: workspace",
            Self::HeaderItem(HeaderItemKind::Model) => "Header: model",
            Self::HeaderItem(HeaderItemKind::Context) => "Header: context",
            Self::HeaderItem(HeaderItemKind::Cache) => "Header: cache",
            Self::HeaderItem(HeaderItemKind::Rate) => "Header: rate",
            Self::HeaderItem(HeaderItemKind::Tokens) => "Header: tokens",
            Self::MouseScrollLines => "Mouse scroll lines",
            Self::ShowSuccessfulCommandOutput => "Command output preview",
            Self::ActiveProvider => "Provider",
            Self::ActiveModel => "Model",
            Self::ReasoningEffort => "Reasoning effort",
            Self::ServiceTier => "Service tier",
            Self::CompactionProvider => "Compaction provider",
            Self::CompactionModel => "Compaction model",
            Self::CompactionReasoning => "Compaction effort",
            Self::ApprovalReviewProvider => "Review provider",
            Self::ApprovalReviewModel => "Review model",
            Self::ApprovalReviewReasoning => "Review effort",
            Self::ManageProviders => "Providers",
            Self::ContextWindow => "Context window",
            Self::AutoCompaction => "Auto compaction",
            Self::ContextStrategy => "Context strategy",
            Self::RetainedModelTurns => "Retained model turns",
            Self::Subagents => "Subagents",
            Self::MaxThreads => "Max threads",
            Self::SubagentLimitBehavior => "Thread limit policy",
            Self::MaxParallelToolCalls => "Max parallel tools",
            Self::SubagentDefaultProvider => "Subagent provider",
            Self::SubagentDefaultModel => "Subagent model",
            Self::SubagentDefaultReasoning => "Subagent effort",
            Self::SubagentRoleSelector => "Role",
            Self::SubagentRoleEnabled => "  Enabled",
            Self::SubagentRoleProvider => "  Provider",
            Self::SubagentRoleModel => "  Model",
            Self::SubagentRoleReasoning => "  Effort",
            Self::SubagentRoleMaxTurns => "  Max turns",
            Self::SubagentRoleTimeout => "  Timeout",
            Self::PermissionSandbox => "Sandbox",
            Self::PermissionMode => "Permission mode",
            Self::PermissionAutoFallback => "Auto fallback",
            Self::PermissionNetwork => "Network access",
            Self::PermissionSshAgent => "SSH agent",
            Self::PermissionGpgAgent => "GPG agent",
            Self::PermissionDbus => "Session D-Bus",
            Self::Shortcut(KeyAction::Submit) => "Submit",
            Self::Shortcut(KeyAction::InsertNewline) => "Insert newline",
            Self::Shortcut(KeyAction::PasteImage) => "Paste image",
            Self::Shortcut(KeyAction::TogglePlan) => "Toggle plan",
            Self::Shortcut(KeyAction::OpenCommandDetails) => "Command output",
            Self::Shortcut(KeyAction::FollowLatest) => "Follow latest",
            #[cfg(feature = "web")]
            Self::Shortcut(KeyAction::OpenSessionInBrowser) => "Open in browser",
            Self::Shortcut(KeyAction::ReviewPreviousUserInput) => "Previous user input",
            Self::Shortcut(KeyAction::Interrupt) => "Interrupt run",
            Self::Shortcut(KeyAction::CancelInputOrQuit) => "Clear / quit",
            Self::Shortcut(KeyAction::Quit) => "Quit",
            Self::Shortcut(KeyAction::ScrollUp) => "Scroll up",
            Self::Shortcut(KeyAction::ScrollDown) => "Scroll down",
            Self::Shortcut(KeyAction::HistoryPrevious) => "History previous",
            Self::Shortcut(KeyAction::HistoryNext) => "History next",
            Self::Shortcut(KeyAction::HistorySearch) => "History search",
            Self::Shortcut(KeyAction::ToggleComment) => "Toggle comment",
            Self::Shortcut(KeyAction::OpenCommandPanel) => "Command palette",
            Self::Shortcut(KeyAction::SendComments) => "Send comments",
        }
    }

    pub(crate) const fn description(self) -> &'static str {
        match self {
            Self::CodeTheme => "Syntax highlight theme for fenced code blocks in the timeline.",
            Self::UiTheme => "Color palette preset for the TUI cockpit and modals.",
            Self::TransparentBackground => {
                "Clear main canvas & status background to inherit your terminal wallpaper."
            }
            Self::Animations => "Enable smooth timeline scroll and activity shimmer animations.",
            Self::BrandMark => "Header brand glyph style (emoji or snowflake).",
            Self::MaxReadingWidth => {
                "Maximum column width for prose wrapping (0 = unlimited, or 40..240)."
            }
            Self::TextSizing => "OSC 66 terminal text sizing protocol for headings.",
            Self::HeaderItem(_) => {
                "Toggle whether this pill appears in the top header bar. Reset restores defaults."
            }
            Self::MouseScrollLines => "Lines scrolled per coalesced mouse-wheel notch (1..10).",
            Self::ShowSuccessfulCommandOutput => {
                "Display bounded output previews for successful shell commands and file reads."
            }
            Self::ActiveProvider => {
                "Last-used provider remembered across sessions (Enter manages providers)."
            }
            Self::ActiveModel => {
                "Model for the active provider (Enter searches API or accepts custom ID)."
            }
            Self::ReasoningEffort => {
                "Reasoning effort for the active provider (Left/Right presets, Enter custom)."
            }
            Self::ServiceTier => {
                "OpenAI-compatible service_tier priority hint for the active provider."
            }
            Self::CompactionProvider => {
                "Provider for context compaction (applies live; Reset inherits primary)."
            }
            Self::CompactionModel => {
                "Model for context compaction (Enter searches API or custom ID; applies live)."
            }
            Self::CompactionReasoning => {
                "Reasoning effort for context compaction (Left/Right presets, Enter custom)."
            }
            Self::ApprovalReviewProvider => {
                "Provider for AI permission review (applies live; Reset inherits primary)."
            }
            Self::ApprovalReviewModel => {
                "Model for AI permission review (Enter searches API or custom ID; applies live)."
            }
            Self::ApprovalReviewReasoning => {
                "Reasoning effort for AI permission review (Left/Right presets, Enter custom)."
            }
            Self::ManageProviders => {
                "Add, edit, copy API keys, test connections, or delete saved providers."
            }
            Self::ContextWindow => {
                "Override effective context window tokens used for compaction thresholds."
            }
            Self::AutoCompaction => "Automatically compact older history before hitting limits.",
            Self::ContextStrategy => "How aggressively automatic compaction summarizes old turns.",
            Self::RetainedModelTurns => "Recent model turns kept verbatim after compaction.",
            Self::Subagents => "Allow spawning delegated child subagents during this session.",
            Self::MaxThreads => "Maximum concurrent subagent threads allowed at once.",
            Self::SubagentLimitBehavior => "Whether new subagents queue or fail at the thread cap.",
            Self::MaxParallelToolCalls => {
                "Max parallel-safe tool calls per turn (1..32; applies live)."
            }
            Self::SubagentDefaultProvider => {
                "Default provider for spawned subagents (Reset inherits primary)."
            }
            Self::SubagentDefaultModel => {
                "Default model for spawned subagents (Enter searches API or custom ID)."
            }
            Self::SubagentDefaultReasoning => {
                "Default reasoning effort for spawned subagents (Left/Right or Enter)."
            }
            Self::SubagentRoleSelector => {
                "Choose which subagent role to configure below (Left/Right or Enter)."
            }
            Self::SubagentRoleEnabled => "Enable or disable the selected subagent role.",
            Self::SubagentRoleProvider => {
                "Provider override for the selected subagent role (Reset inherits default)."
            }
            Self::SubagentRoleModel => {
                "Model override for the selected subagent role (Enter searches API or custom ID)."
            }
            Self::SubagentRoleReasoning => {
                "Reasoning effort override for the selected subagent role (Left/Right or Enter)."
            }
            Self::SubagentRoleMaxTurns => {
                "Maximum model turns per run for the selected subagent role."
            }
            Self::SubagentRoleTimeout => {
                "Wall-clock timeout in seconds for the selected subagent role."
            }
            Self::PermissionSandbox => {
                "Execution isolation: workspace sandbox or host (CLI flags take precedence)."
            }
            Self::PermissionMode => {
                "Tool approval policy: auto / ask / always-approve / deny (applies live)."
            }
            Self::PermissionAutoFallback => {
                "Fallback when auto AI review denies or cannot decide: ask or deny (applies live)."
            }
            Self::PermissionNetwork => {
                "Allow outbound network access inside the workspace sandbox."
            }
            Self::PermissionSshAgent => {
                "Forward SSH_AUTH_SOCK into the workspace sandbox when available."
            }
            Self::PermissionGpgAgent => {
                "Forward GPG agent socket into the workspace sandbox when available."
            }
            Self::PermissionDbus => {
                "Forward session D-Bus socket into the workspace sandbox when available."
            }
            Self::Shortcut(_) => {
                "Press Enter and press a key combo to rebind live (r resets to default)."
            }
        }
    }

    pub(crate) const fn is_next_session(self) -> bool {
        matches!(
            self,
            Self::SubagentDefaultProvider
                | Self::SubagentDefaultModel
                | Self::SubagentDefaultReasoning
                | Self::SubagentRoleEnabled
                | Self::SubagentRoleProvider
                | Self::SubagentRoleModel
                | Self::SubagentRoleReasoning
                | Self::SubagentRoleMaxTurns
                | Self::SubagentRoleTimeout
                | Self::PermissionSandbox
                | Self::PermissionNetwork
                | Self::PermissionSshAgent
                | Self::PermissionGpgAgent
                | Self::PermissionDbus
        )
    }

    pub(crate) const fn opens_choice_menu(self) -> bool {
        matches!(
            self,
            Self::CodeTheme
                | Self::UiTheme
                | Self::BrandMark
                | Self::TextSizing
                | Self::MouseScrollLines
                | Self::ServiceTier
                | Self::CompactionProvider
                | Self::ApprovalReviewProvider
                | Self::ContextStrategy
                | Self::RetainedModelTurns
                | Self::MaxThreads
                | Self::SubagentLimitBehavior
                | Self::MaxParallelToolCalls
                | Self::SubagentDefaultProvider
                | Self::SubagentRoleSelector
                | Self::SubagentRoleProvider
                | Self::SubagentRoleMaxTurns
                | Self::SubagentRoleTimeout
                | Self::PermissionSandbox
                | Self::PermissionMode
                | Self::PermissionAutoFallback
        )
    }

    pub(crate) const fn is_action_only(self) -> bool {
        matches!(self, Self::ManageProviders)
    }

    pub(crate) const fn is_ui_only(self) -> bool {
        self.is_next_session()
            || matches!(
                self,
                Self::CodeTheme
                    | Self::UiTheme
                    | Self::TransparentBackground
                    | Self::Animations
                    | Self::BrandMark
                    | Self::MaxReadingWidth
                    | Self::TextSizing
                    | Self::HeaderItem(_)
                    | Self::MouseScrollLines
                    | Self::ShowSuccessfulCommandOutput
                    | Self::Shortcut(_)
            )
    }

    pub(crate) const fn opens_inline_editor(self) -> bool {
        matches!(self, Self::ContextWindow | Self::MaxReadingWidth)
    }

    pub(crate) const fn opens_model_picker(self) -> bool {
        matches!(
            self,
            Self::ActiveModel
                | Self::CompactionModel
                | Self::ApprovalReviewModel
                | Self::SubagentDefaultModel
                | Self::SubagentRoleModel
        )
    }

    pub(crate) const fn opens_reasoning_picker(self) -> bool {
        matches!(
            self,
            Self::ReasoningEffort
                | Self::CompactionReasoning
                | Self::ApprovalReviewReasoning
                | Self::SubagentDefaultReasoning
                | Self::SubagentRoleReasoning
        )
    }
}
