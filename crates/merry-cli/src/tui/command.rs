use super::keymap::{KeyAction, Keymap};

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
    SaveSession,
    Quit,
    TogglePlanMode,
    ViewPlans,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CommandSpec {
    pub(crate) command: PaletteCommand,
    pub(crate) category: &'static str,
    pub(crate) label: &'static str,
    pub(crate) key_action: Option<KeyAction>,
    slash_name: Option<&'static str>,
    slash_description: Option<&'static str>,
    accepts_args: bool,
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
            slash_description: None,
            accepts_args: false,
        }
    }

    const fn with_slash(mut self, name: &'static str, description: &'static str) -> Self {
        self.slash_name = Some(name);
        self.slash_description = Some(description);
        self
    }

    const fn with_slash_args(mut self, name: &'static str, description: &'static str) -> Self {
        self.slash_name = Some(name);
        self.slash_description = Some(description);
        self.accepts_args = true;
        self
    }

    pub(crate) const fn slash_name(&self) -> Option<&'static str> {
        self.slash_name
    }

    pub(crate) const fn slash_description(&self) -> Option<&'static str> {
        self.slash_description
    }
}

const COMMANDS: &[CommandSpec] = &[
    CommandSpec::new(PaletteCommand::OpenSettings, "Merry", "Settings", None),
    CommandSpec::new(
        PaletteCommand::OpenProviders,
        "Merry",
        "Providers & models",
        None,
    ),
    CommandSpec::new(
        PaletteCommand::ShowShortcuts,
        "Merry",
        "Keyboard shortcuts",
        None,
    ),
    CommandSpec::new(PaletteCommand::ShowHelp, "Merry", "Command help", None)
        .with_slash("help", "List slash commands and keys"),
    #[cfg(feature = "web")]
    CommandSpec::new(
        PaletteCommand::OpenSessionInBrowser,
        "Web",
        "Open trajectory in browser",
        Some(KeyAction::OpenSessionInBrowser),
    ),
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
    .with_slash("exit", "Quit Merry"),
    CommandSpec::new(
        PaletteCommand::TogglePlanMode,
        "Plan",
        "Toggle Plan Mode",
        None,
    )
    .with_slash_args("plan", "Toggle Plan Mode or start planning a prompt"),
    CommandSpec::new(
        PaletteCommand::ViewPlans,
        "Plan",
        "View & review plans",
        Some(KeyAction::TogglePlan),
    )
    .with_slash("plans", "Open the Plan Review & History view"),
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

pub(crate) fn slash_commands() -> Vec<&'static CommandSpec> {
    find_slash_prefix("")
}

pub(crate) fn find_slash_exact(name: &str) -> Option<&'static CommandSpec> {
    COMMANDS
        .iter()
        .find(|command| command.slash_name() == Some(name))
}

pub(crate) fn find_slash_prefix(query: &str) -> Vec<&'static CommandSpec> {
    let mut commands = COMMANDS
        .iter()
        .filter(|command| {
            command
                .slash_name()
                .is_some_and(|name| name.starts_with(query))
        })
        .collect::<Vec<_>>();
    commands.sort_by_key(|command| command.slash_name());
    commands
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
        (Some(command), false) if command.accepts_args => {
            SlashCommandMatch::Known(command, Some(arguments))
        }
        (Some(_), false) => SlashCommandMatch::ArgumentsNotSupported(name),
        (None, _) => SlashCommandMatch::Unknown(name),
    }
}

pub(crate) fn slash_help_body(keymap: &Keymap) -> String {
    let mut lines = slash_commands()
        .into_iter()
        .filter_map(|command| {
            Some(format!(
                "/{}  {}",
                command.slash_name()?,
                command.slash_description()?
            ))
        })
        .collect::<Vec<_>>();
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
    fn slash_registry_is_unique_and_queryable() {
        let commands = slash_commands();
        assert!(!commands.is_empty());
        let names = commands
            .iter()
            .map(|command| command.slash_name().expect("slash name"))
            .collect::<BTreeSet<_>>();
        assert_eq!(names.len(), commands.len());
        assert!(names.iter().all(|name| !name.starts_with('/')));
        assert_eq!(
            find_slash_exact("save").map(|command| command.command),
            Some(PaletteCommand::SaveSession)
        );
        assert_eq!(
            find_slash_prefix("sa")
                .into_iter()
                .filter_map(CommandSpec::slash_name)
                .collect::<Vec<_>>(),
            vec!["save"]
        );
        assert_eq!(find_slash_prefix("").len(), 8);
        assert_eq!(
            find_slash_exact("tasks").map(|command| command.command),
            Some(PaletteCommand::ShowTasks)
        );
        assert_eq!(
            find_slash_exact("plan").map(|command| command.command),
            Some(PaletteCommand::TogglePlanMode)
        );
        assert_eq!(
            find_slash_exact("plans").map(|command| command.command),
            Some(PaletteCommand::ViewPlans)
        );
        assert!(find_slash_exact("unknown").is_none());
    }

    #[test]
    fn slash_parser_is_strict_without_consuming_normal_paths_or_multiline_text() {
        assert!(matches!(
            match_slash_input("/save   "),
            SlashCommandMatch::Known(command, None) if command.command == PaletteCommand::SaveSession
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
