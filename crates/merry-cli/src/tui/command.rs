use super::keymap::{KeyAction, Keymap};
use merry::profiles::CodingWorkflowCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaletteCommand {
    OpenSettings,
    OpenProviders,
    ShowShortcuts,
    ShowHelp,
    ShowStatus,
    ShowTasks,
    #[cfg(feature = "web")]
    OpenSessionInBrowser,
    ReviewPreviousUserInput,
    OpenCommandDetails,
    FollowLatest,
    Interrupt,
    NewSession,
    ResumeSession,
    CompactContext,
    SaveSession,
    Quit,
    TogglePlanMode,
    ViewPlans,
    OpenSkills,
    CodeReview,
    SecurityReview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CommandSpec {
    pub(crate) command: PaletteCommand,
    pub(crate) category: &'static str,
    pub(crate) label: &'static str,
    pub(crate) key_action: Option<KeyAction>,
    slash_name: Option<&'static str>,
    slash_aliases: &'static [&'static str],
    slash_description: Option<&'static str>,
    arg_hint: Option<&'static str>,
}

impl CommandSpec {
    const fn new(
        command: PaletteCommand,
        category: &'static str,
        label: &'static str,
        key_action: Option<KeyAction>,
    ) -> Self {
        Self {
            command,
            category,
            label,
            key_action,
            slash_name: None,
            slash_aliases: &[],
            slash_description: None,
            arg_hint: None,
        }
    }

    const fn with_slash(mut self, name: &'static str, description: &'static str) -> Self {
        self.slash_name = Some(name);
        self.slash_description = Some(description);
        self
    }

    const fn with_slash_aliases(mut self, aliases: &'static [&'static str]) -> Self {
        self.slash_aliases = aliases;
        self
    }

    const fn with_slash_args(
        mut self,
        name: &'static str,
        arg_hint: &'static str,
        description: &'static str,
    ) -> Self {
        self.slash_name = Some(name);
        self.slash_description = Some(description);
        self.arg_hint = Some(arg_hint);
        self
    }

    pub(crate) const fn slash_name(&self) -> Option<&'static str> {
        self.slash_name
    }

    pub(crate) const fn slash_aliases(&self) -> &'static [&'static str] {
        self.slash_aliases
    }

    pub(crate) fn all_slash_names(&self) -> impl Iterator<Item = &'static str> {
        self.slash_name
            .into_iter()
            .chain(self.slash_aliases.iter().copied())
    }

    pub(crate) const fn slash_description(&self) -> Option<&'static str> {
        self.slash_description
    }

    pub(crate) const fn arg_hint(&self) -> Option<&'static str> {
        self.arg_hint
    }

    pub(crate) const fn accepts_args(&self) -> bool {
        self.arg_hint.is_some()
    }
}

const COMMANDS: &[CommandSpec] = &[
    CommandSpec::new(PaletteCommand::OpenSettings, "Merry", "Settings", None)
        .with_slash("settings", "Open settings")
        .with_slash_aliases(&["config"]),
    CommandSpec::new(
        PaletteCommand::OpenProviders,
        "Merry",
        "Providers & models",
        None,
    )
    .with_slash("model", "Switch provider or model")
    .with_slash_aliases(&["provider", "providers"]),
    CommandSpec::new(
        PaletteCommand::ShowShortcuts,
        "Merry",
        "Keyboard shortcuts",
        None,
    )
    .with_slash("shortcuts", "Show keyboard shortcuts")
    .with_slash_aliases(&["keys"]),
    CommandSpec::new(PaletteCommand::ShowHelp, "Merry", "Command help", None)
        .with_slash("help", "List slash commands and keys"),
    #[cfg(feature = "web")]
    CommandSpec::new(
        PaletteCommand::OpenSessionInBrowser,
        "Web",
        "Open trajectory in browser",
        Some(KeyAction::OpenSessionInBrowser),
    )
    .with_slash("web", "Open trajectory in browser"),
    CommandSpec::new(
        PaletteCommand::ReviewPreviousUserInput,
        "Navigation",
        "Previous user input",
        Some(KeyAction::ReviewPreviousUserInput),
    ),
    CommandSpec::new(
        PaletteCommand::OpenCommandDetails,
        "Navigation",
        "Inspect command output",
        Some(KeyAction::OpenCommandDetails),
    ),
    CommandSpec::new(
        PaletteCommand::FollowLatest,
        "Navigation",
        "Follow latest output",
        Some(KeyAction::FollowLatest),
    ),
    CommandSpec::new(
        PaletteCommand::Interrupt,
        "Runtime",
        "Interrupt current run",
        Some(KeyAction::Interrupt),
    )
    .with_slash("stop", "Interrupt the active run"),
    CommandSpec::new(
        PaletteCommand::NewSession,
        "Session",
        "New session",
        None,
    )
    .with_slash("new", "Start a fresh session")
    .with_slash_aliases(&["clear"]),
    CommandSpec::new(
        PaletteCommand::ResumeSession,
        "Session",
        "Resume session",
        None,
    )
    .with_slash("resume", "Switch to a previous session"),
    CommandSpec::new(
        PaletteCommand::CompactContext,
        "Session",
        "Compact context",
        None,
    )
    .with_slash_args(
        "compact",
        "[focus]",
        "Compact conversation history into a checkpoint",
    ),
    CommandSpec::new(
        PaletteCommand::ShowStatus,
        "Session",
        "Show session status",
        None,
    )
    .with_slash(
        "status",
        "Show run, model, usage, plan, and workspace state",
    ),
    CommandSpec::new(PaletteCommand::ShowTasks, "Session", "Tasks", None)
        .with_slash("tasks", "Processes and subagents"),
    CommandSpec::new(PaletteCommand::SaveSession, "Session", "Save session", None)
        .with_slash("save", "Save the session at a stable boundary"),
    CommandSpec::new(
        PaletteCommand::Quit,
        "Session",
        "Quit Merry",
        Some(KeyAction::Quit),
    )
    .with_slash("exit", "Quit Merry")
    .with_slash_aliases(&["quit"]),
    CommandSpec::new(
        PaletteCommand::TogglePlanMode,
        "Workflow",
        "Toggle Plan Mode",
        None,
    )
    .with_slash_args(
        "plan",
        "[prompt]",
        "Toggle Plan Mode or start planning a prompt",
    ),
    CommandSpec::new(
        PaletteCommand::ViewPlans,
        "Workflow",
        "View & review plans",
        Some(KeyAction::TogglePlan),
    )
    .with_slash("plans", "Open the Plan Review & History view"),
    CommandSpec::new(PaletteCommand::OpenSkills, "Workflow", "Skills", None)
        .with_slash("skills", "Browse and insert a skill ($skill-name)")
        .with_slash_aliases(&["skill"]),
    CommandSpec::new(
        PaletteCommand::CodeReview,
        "Workflow",
        "Code review",
        None,
    )
    .with_slash_args(
        CodingWorkflowCommand::CodeReview.name(),
        "[scope]",
        CodingWorkflowCommand::CodeReview.description(),
    )
    .with_slash_aliases(CodingWorkflowCommand::CodeReview.aliases()),
    CommandSpec::new(
        PaletteCommand::SecurityReview,
        "Workflow",
        "Security review",
        None,
    )
    .with_slash_args(
        CodingWorkflowCommand::SecurityReview.name(),
        "[scope]",
        CodingWorkflowCommand::SecurityReview.description(),
    )
    .with_slash_aliases(CodingWorkflowCommand::SecurityReview.aliases()),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlashCommandMatch<'a> {
    NotCommand,
    Known(&'static CommandSpec, Option<&'a str>),
    Unknown(&'a str),
    ArgumentsNotSupported(&'a str),
}

pub(crate) fn all_commands() -> &'static [CommandSpec] {
    COMMANDS
}

pub(crate) fn find_slash_exact(name: &str) -> Option<&'static CommandSpec> {
    COMMANDS
        .iter()
        .find(|command| command.all_slash_names().any(|candidate| candidate == name))
}

/// Returns slash commands matching `query`, ranked by:
/// 0. canonical name prefix
/// 1. alias prefix
/// 2. canonical name or alias substring
/// 3. fuzzy subsequence match on canonical name or alias (e.g. `cr` -> `code-review`)
pub(crate) fn find_slash_matches(query: &str) -> Vec<&'static CommandSpec> {
    let query = query.to_ascii_lowercase();
    let mut scored = COMMANDS
        .iter()
        .filter_map(|command| {
            let primary = command.slash_name()?;
            let (rank, fuzzy_score) = if query.is_empty() || primary.starts_with(&query) {
                (0_u8, 0_i32)
            } else if command
                .slash_aliases()
                .iter()
                .any(|alias| alias.starts_with(&query))
            {
                (1_u8, 0_i32)
            } else if command
                .all_slash_names()
                .any(|candidate| candidate.contains(&query))
            {
                (2_u8, 0_i32)
            } else {
                let best_fuzzy = command
                    .all_slash_names()
                    .filter_map(|candidate| super::fuzzy::fuzzy_match(candidate, &query))
                    .map(|matched| matched.score)
                    .max()?;
                (3_u8, best_fuzzy)
            };
            Some((rank, std::cmp::Reverse(fuzzy_score), primary, command))
        })
        .collect::<Vec<_>>();
    scored.sort_by_key(|(rank, score, primary, _)| (*rank, *score, *primary));
    scored.into_iter().map(|(_, _, _, command)| command).collect()
}

pub(crate) fn match_slash_input(input: &str) -> SlashCommandMatch<'_> {
    if input.contains(['\n', '\r']) {
        return SlashCommandMatch::NotCommand;
    }
    let Some(rest) = input.trim_end().strip_prefix('/') else {
        return SlashCommandMatch::NotCommand;
    };
    let name_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let name = &rest[..name_end];
    if !name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return SlashCommandMatch::NotCommand;
    }
    let arguments = rest[name_end..].trim();
    match (find_slash_exact(name), arguments.is_empty()) {
        (Some(command), true) => SlashCommandMatch::Known(command, None),
        (Some(command), false) if command.accepts_args() => {
            SlashCommandMatch::Known(command, Some(arguments))
        }
        (Some(_), false) => SlashCommandMatch::ArgumentsNotSupported(name),
        (None, _) => SlashCommandMatch::Unknown(name),
    }
}

pub(crate) fn slash_help_body(keymap: &Keymap) -> String {
    let mut lines = vec![
        "Session   /new (/clear) · /resume · /save · /stop · /exit (/quit)".to_owned(),
        "Context   /compact [focus] · /status · /tasks · /skills ($skill)".to_owned(),
        "Workflow  /plan [prompt] · /plans · /code-review (/review) · /security-review".to_owned(),
    ];
    #[cfg(feature = "web")]
    lines.push(
        "Config    /model (/provider) · /settings (/config) · /shortcuts · /web · /help".to_owned(),
    );
    #[cfg(not(feature = "web"))]
    lines.push("Config    /model (/provider) · /settings (/config) · /shortcuts · /help".to_owned());
    lines.push("Type / to search commands with descriptions.".to_owned());
    lines.push(format!(
        "Submit {}  Commands {}  Stop {}",
        binding_label(keymap, KeyAction::Submit),
        binding_label(keymap, KeyAction::OpenCommandPanel),
        binding_label(keymap, KeyAction::Interrupt),
    ));
    lines.join("\n")
}

fn binding_label(keymap: &Keymap, action: KeyAction) -> String {
    keymap
        .binding_label_for(action)
        .unwrap_or_else(|| "Unbound".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn slash_registry_is_unique_across_names_and_aliases_and_queryable() {
        let commands = find_slash_matches("");
        assert!(!commands.is_empty());
        let mut all_names = BTreeSet::new();
        for command in &commands {
            for name in command.all_slash_names() {
                assert!(!name.starts_with('/'));
                assert!(
                    all_names.insert(name),
                    "duplicate slash name or alias: {name}"
                );
            }
        }

        assert_eq!(
            find_slash_exact("new").map(|command| command.command),
            Some(PaletteCommand::NewSession)
        );
        assert_eq!(
            find_slash_exact("clear").map(|command| command.command),
            Some(PaletteCommand::NewSession)
        );
        assert_eq!(
            find_slash_exact("compact").map(|command| command.command),
            Some(PaletteCommand::CompactContext)
        );
        assert_eq!(
            find_slash_exact("code-review").map(|command| command.command),
            Some(PaletteCommand::CodeReview)
        );
        assert_eq!(
            find_slash_exact("review").map(|command| command.command),
            Some(PaletteCommand::CodeReview)
        );
        assert_eq!(
            find_slash_exact("security-review").map(|command| command.command),
            Some(PaletteCommand::SecurityReview)
        );
        assert_eq!(
            find_slash_exact("sec-review").map(|command| command.command),
            Some(PaletteCommand::SecurityReview)
        );
        assert_eq!(
            find_slash_exact("model").map(|command| command.command),
            Some(PaletteCommand::OpenProviders)
        );
        assert_eq!(
            find_slash_exact("provider").map(|command| command.command),
            Some(PaletteCommand::OpenProviders)
        );
        assert_eq!(
            find_slash_exact("resume").map(|command| command.command),
            Some(PaletteCommand::ResumeSession)
        );
        assert_eq!(
            find_slash_exact("settings").map(|command| command.command),
            Some(PaletteCommand::OpenSettings)
        );
        assert_eq!(
            find_slash_exact("config").map(|command| command.command),
            Some(PaletteCommand::OpenSettings)
        );
        assert_eq!(
            find_slash_exact("quit").map(|command| command.command),
            Some(PaletteCommand::Quit)
        );
        assert_eq!(
            find_slash_exact("shortcuts").map(|command| command.command),
            Some(PaletteCommand::ShowShortcuts)
        );
        assert_eq!(
            find_slash_exact("keys").map(|command| command.command),
            Some(PaletteCommand::ShowShortcuts)
        );
        assert!(find_slash_exact("unknown").is_none());

        assert_eq!(
            find_slash_matches("sa").first().and_then(|c| c.slash_name()),
            Some("save")
        );
        assert_eq!(
            find_slash_matches("sav")
                .into_iter()
                .filter_map(CommandSpec::slash_name)
                .collect::<Vec<_>>(),
            vec!["save"]
        );
        assert_eq!(
            find_slash_matches("cr").first().and_then(|c| c.slash_name()),
            Some("code-review")
        );
        // Alias prefix and substring matching surface review commands naturally.
        assert_eq!(
            find_slash_matches("rev")
                .into_iter()
                .filter_map(CommandSpec::slash_name)
                .collect::<Vec<_>>(),
            vec!["code-review", "security-review"]
        );
        assert_eq!(
            find_slash_matches("clear")
                .into_iter()
                .filter_map(CommandSpec::slash_name)
                .collect::<Vec<_>>(),
            vec!["new"]
        );
    }

    #[test]
    fn slash_parser_is_strict_without_consuming_normal_paths_or_multiline_text() {
        assert!(matches!(
            match_slash_input("/save   "),
            SlashCommandMatch::Known(command, None) if command.command == PaletteCommand::SaveSession
        ));
        assert!(matches!(
            match_slash_input("/clear"),
            SlashCommandMatch::Known(command, None) if command.command == PaletteCommand::NewSession
        ));
        assert!(matches!(
            match_slash_input("/compact keep auth details"),
            SlashCommandMatch::Known(command, Some("keep auth details"))
                if command.command == PaletteCommand::CompactContext
        ));
        assert!(matches!(
            match_slash_input("/review staged"),
            SlashCommandMatch::Known(command, Some("staged"))
                if command.command == PaletteCommand::CodeReview
        ));
        assert!(matches!(
            match_slash_input("/security-review src/auth.rs"),
            SlashCommandMatch::Known(command, Some("src/auth.rs"))
                if command.command == PaletteCommand::SecurityReview
        ));
        assert!(matches!(
            match_slash_input("/plan refactor auth"),
            SlashCommandMatch::Known(command, Some("refactor auth"))
                if command.command == PaletteCommand::TogglePlanMode
        ));
        assert_eq!(
            match_slash_input("/unknown"),
            SlashCommandMatch::Unknown("unknown")
        );
        assert_eq!(
            match_slash_input("/save now"),
            SlashCommandMatch::ArgumentsNotSupported("save")
        );
        assert_eq!(
            match_slash_input("/clear now"),
            SlashCommandMatch::ArgumentsNotSupported("clear")
        );
        assert_eq!(
            match_slash_input("/unknown now"),
            SlashCommandMatch::Unknown("unknown")
        );
        assert_eq!(
            match_slash_input("/tmp/file"),
            SlashCommandMatch::NotCommand
        );
        assert_eq!(match_slash_input("/tmp"), SlashCommandMatch::Unknown("tmp"));
        assert_eq!(
            match_slash_input("/save\nexplain why"),
            SlashCommandMatch::NotCommand
        );
    }
}
