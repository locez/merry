use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum KeyAction {
    Submit,
    CancelInputOrQuit,
    InsertNewline,
    PasteImage,
    #[cfg(feature = "web")]
    OpenSessionInBrowser,
    Interrupt,
    OpenCommandPanel,
    OpenCommandDetails,
    TogglePlan,
    Quit,
    ScrollUp,
    ScrollDown,
    ReviewPreviousUserInput,
    JumpToLatestResponse,
    HistoryPrevious,
    HistoryNext,
    HistorySearch,
    ToggleComment,
    SendComments,
}

impl KeyAction {
    pub(crate) fn config_key(self) -> Option<&'static str> {
        match self {
            Self::Submit => Some("submit"),
            Self::InsertNewline => Some("insert_newline"),
            Self::PasteImage => Some("paste_image"),
            Self::TogglePlan => Some("toggle_plan"),
            Self::OpenCommandDetails => Some("open_command_details"),
            #[cfg(feature = "web")]
            Self::OpenSessionInBrowser => Some("open_session_in_browser"),
            Self::ReviewPreviousUserInput => Some("review_previous_user_input"),
            Self::JumpToLatestResponse => Some("jump_to_latest_response"),
            Self::Interrupt => Some("interrupt"),
            Self::CancelInputOrQuit => Some("cancel_input_or_quit"),
            Self::Quit => Some("quit"),
            Self::ScrollUp => Some("scroll_up"),
            Self::ScrollDown => Some("scroll_down"),
            Self::HistoryPrevious => Some("history_previous"),
            Self::HistoryNext => Some("history_next"),
            Self::HistorySearch => Some("history_search"),
            Self::ToggleComment => Some("toggle_comment"),
            Self::OpenCommandPanel | Self::SendComments => None,
        }
    }

    pub(crate) fn from_config_key(key: &str) -> Option<Self> {
        match key {
            "submit" => Some(Self::Submit),
            "insert_newline" => Some(Self::InsertNewline),
            "paste_image" => Some(Self::PasteImage),
            "toggle_plan" => Some(Self::TogglePlan),
            "open_command_details" => Some(Self::OpenCommandDetails),
            #[cfg(feature = "web")]
            "open_session_in_browser" => Some(Self::OpenSessionInBrowser),
            "review_previous_user_input" => Some(Self::ReviewPreviousUserInput),
            "jump_to_latest_response" => Some(Self::JumpToLatestResponse),
            "interrupt" => Some(Self::Interrupt),
            "cancel_input_or_quit" => Some(Self::CancelInputOrQuit),
            "quit" => Some(Self::Quit),
            "scroll_up" => Some(Self::ScrollUp),
            "scroll_down" => Some(Self::ScrollDown),
            "history_previous" => Some(Self::HistoryPrevious),
            "history_next" => Some(Self::HistoryNext),
            "history_search" => Some(Self::HistorySearch),
            "toggle_comment" => Some(Self::ToggleComment),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct KeyBinding {
    code: KeyCode,
    modifiers: KeyModifiers,
}

impl KeyBinding {
    pub(crate) fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }
}

impl From<KeyEvent> for KeyBinding {
    fn from(event: KeyEvent) -> Self {
        Self {
            code: event.code,
            modifiers: event.modifiers,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Keymap {
    bindings: Vec<(KeyBinding, KeyAction)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            bindings: vec![
                (
                    KeyBinding::new(KeyCode::Enter, KeyModifiers::NONE),
                    KeyAction::Submit,
                ),
                (
                    KeyBinding::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                    KeyAction::CancelInputOrQuit,
                ),
                (
                    KeyBinding::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
                    KeyAction::InsertNewline,
                ),
                (
                    KeyBinding::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
                    KeyAction::PasteImage,
                ),
                (
                    KeyBinding::new(KeyCode::Char('x'), KeyModifiers::ALT),
                    KeyAction::OpenCommandPanel,
                ),
                (
                    KeyBinding::new(KeyCode::Char('t'), KeyModifiers::ALT),
                    KeyAction::OpenCommandDetails,
                ),
                (
                    KeyBinding::new(KeyCode::Char('p'), KeyModifiers::ALT),
                    KeyAction::TogglePlan,
                ),
                (
                    KeyBinding::new(KeyCode::Esc, KeyModifiers::NONE),
                    KeyAction::Interrupt,
                ),
                (
                    KeyBinding::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
                    KeyAction::Quit,
                ),
                (
                    KeyBinding::new(KeyCode::PageUp, KeyModifiers::NONE),
                    KeyAction::ScrollUp,
                ),
                (
                    KeyBinding::new(KeyCode::PageDown, KeyModifiers::NONE),
                    KeyAction::ScrollDown,
                ),
                (
                    KeyBinding::new(KeyCode::Char('u'), KeyModifiers::ALT),
                    KeyAction::ReviewPreviousUserInput,
                ),
                (
                    KeyBinding::new(KeyCode::Char('r'), KeyModifiers::ALT),
                    KeyAction::JumpToLatestResponse,
                ),
                (
                    KeyBinding::new(KeyCode::Char('c'), KeyModifiers::ALT),
                    KeyAction::ToggleComment,
                ),
                #[cfg(feature = "web")]
                (
                    KeyBinding::new(KeyCode::Char('g'), KeyModifiers::ALT),
                    KeyAction::OpenSessionInBrowser,
                ),
                (
                    KeyBinding::new(KeyCode::Up, KeyModifiers::NONE),
                    KeyAction::HistoryPrevious,
                ),
                (
                    KeyBinding::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
                    KeyAction::HistoryPrevious,
                ),
                (
                    KeyBinding::new(KeyCode::Down, KeyModifiers::NONE),
                    KeyAction::HistoryNext,
                ),
                (
                    KeyBinding::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
                    KeyAction::HistoryNext,
                ),
                (
                    KeyBinding::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
                    KeyAction::HistorySearch,
                ),
            ],
        }
    }
}

impl Keymap {
    pub(crate) fn from_config(
        config: &crate::config::TuiKeymapToml,
    ) -> Result<Self, crate::config::ConfigError> {
        let mut keymap = Self::default();
        if let Some(binding) = config.submit.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::Submit);
        }
        if let Some(binding) = config.cancel_input_or_quit.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::CancelInputOrQuit);
        }
        if let Some(binding) = config.insert_newline.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::InsertNewline);
        }
        if let Some(binding) = config.paste_image.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::PasteImage);
        }
        if let Some(binding) = config.open_session_in_browser.as_deref() {
            let binding = parse_binding(binding)?;
            #[cfg(feature = "web")]
            keymap.set_binding(binding, KeyAction::OpenSessionInBrowser);
            #[cfg(not(feature = "web"))]
            let _ = binding;
        }
        if let Some(binding) = config.toggle_plan.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::TogglePlan);
        }
        if let Some(binding) = config.interrupt.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::Interrupt);
        }
        if let Some(binding) = config.quit.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::Quit);
        }
        if let Some(binding) = config.scroll_up.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::ScrollUp);
        }
        if let Some(binding) = config.scroll_down.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::ScrollDown);
        }
        if let Some(binding) = config.open_command_details.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::OpenCommandDetails);
        }
        if let Some(binding) = config.review_previous_user_input.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::ReviewPreviousUserInput);
        }
        if let Some(binding) = config.jump_to_latest_response.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::JumpToLatestResponse);
        }
        if let Some(binding) = config.history_previous.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::HistoryPrevious);
        }
        if let Some(binding) = config.history_next.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::HistoryNext);
        }
        if let Some(binding) = config.history_search.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::HistorySearch);
        }
        if let Some(binding) = config.toggle_comment.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::ToggleComment);
        }
        // Readline aliases survive a config that only names Up and Down.
        keymap.bind_if_free(
            KeyBinding::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            KeyAction::HistoryPrevious,
        );
        keymap.bind_if_free(
            KeyBinding::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
            KeyAction::HistoryNext,
        );
        Ok(keymap)
    }

    pub(crate) fn with_overrides(
        &self,
        overrides: &std::collections::BTreeMap<String, String>,
    ) -> Self {
        if overrides.is_empty() {
            return self.clone();
        }
        let mut keymap = self.clone();
        for (key, value) in overrides {
            if let (Some(action), Ok(binding)) =
                (KeyAction::from_config_key(key), parse_binding(value))
            {
                keymap.set_binding(binding, action);
            }
        }
        keymap.bind_if_free(
            KeyBinding::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            KeyAction::HistoryPrevious,
        );
        keymap.bind_if_free(
            KeyBinding::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
            KeyAction::HistoryNext,
        );
        keymap
    }

    fn bind_if_free(&mut self, binding: KeyBinding, action: KeyAction) {
        if self.action_for(binding).is_none() {
            self.bindings.insert(0, (binding, action));
        }
    }

    pub(crate) fn action_for(&self, binding: KeyBinding) -> Option<KeyAction> {
        self.bindings
            .iter()
            .rev()
            .find_map(|(candidate, action)| (*candidate == binding).then_some(*action))
    }

    pub(crate) fn binding_label_for(&self, action: KeyAction) -> Option<String> {
        self.bindings
            .iter()
            .rev()
            .find_map(|(binding, candidate)| (*candidate == action).then(|| binding.label()))
    }

    fn set_binding(&mut self, binding: KeyBinding, action: KeyAction) {
        self.bindings
            .retain(|(candidate_binding, candidate_action)| {
                *candidate_action != action && *candidate_binding != binding
            });
        self.bindings.push((binding, action));
    }
}

/// Name of the Alt chord on this platform.
///
/// The binding stores [`KeyModifiers::ALT`]. macOS keyboards call that key
/// Option, and crossterm reports Option as Alt when the terminal sends the
/// modifier.
pub(crate) fn alt_modifier_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Option"
    } else {
        "Alt"
    }
}

#[cfg(test)]
pub(crate) fn alt_chord(key: &str) -> String {
    format!("{}+{key}", alt_modifier_name())
}

/// macOS ABC layout types these glyphs for Option+letter when the terminal
/// does not report Option as Alt. Dead keys such as Option+N and Option+U
/// produce no character until the next key, so those chords still need the
/// terminal to send the modifier.
fn macos_option_letter(glyph: char) -> Option<char> {
    Some(match glyph {
        'ç' => 'c',
        '∂' => 'd',
        #[cfg(feature = "web")]
        '©' => 'g',
        'π' => 'p',
        '†' => 't',
        '≈' => 'x',
        _ => return None,
    })
}

pub(crate) fn command_key_from_option_glyph(key: KeyEvent) -> Option<KeyEvent> {
    if !key.modifiers.is_empty() {
        return None;
    }
    let KeyCode::Char(glyph) = key.code else {
        return None;
    };
    macos_option_letter(glyph).map(|letter| KeyEvent::new(KeyCode::Char(letter), KeyModifiers::ALT))
}

pub(crate) fn normalize_command_key(key: KeyEvent) -> KeyEvent {
    if cfg!(target_os = "macos") {
        command_key_from_option_glyph(key).unwrap_or(key)
    } else {
        key
    }
}

impl KeyBinding {
    pub(crate) fn canonical_from_key_event(key: KeyEvent) -> Option<String> {
        let key = normalize_command_key(key);
        match (key.code, key.modifiers) {
            (KeyCode::Enter, KeyModifiers::NONE) => Some("enter".to_owned()),
            (KeyCode::Enter, KeyModifiers::ALT) => Some("alt+enter".to_owned()),
            (KeyCode::Up, KeyModifiers::NONE) => Some("up".to_owned()),
            (KeyCode::Down, KeyModifiers::NONE) => Some("down".to_owned()),
            (KeyCode::PageUp, KeyModifiers::NONE) => Some("pageup".to_owned()),
            (KeyCode::PageDown, KeyModifiers::NONE) => Some("pagedown".to_owned()),
            (KeyCode::Char(ch), mods) if ch.is_ascii_alphabetic() => {
                let mods = mods - KeyModifiers::SHIFT;
                let lower = ch.to_ascii_lowercase();
                match mods {
                    KeyModifiers::CONTROL => Some(format!("ctrl+{lower}")),
                    KeyModifiers::ALT if lower != 'x' => Some(format!("alt+{lower}")),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    pub(crate) fn label(self) -> String {
        let mut parts = Vec::new();
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            parts.push("Ctrl".to_owned());
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            parts.push(alt_modifier_name().to_owned());
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            parts.push("Shift".to_owned());
        }
        parts.push(match self.code {
            KeyCode::Char(value) => value.to_ascii_uppercase().to_string(),
            KeyCode::Enter => "Enter".to_owned(),
            KeyCode::Esc => "Esc".to_owned(),
            KeyCode::Up => "Up".to_owned(),
            KeyCode::Down => "Down".to_owned(),
            KeyCode::PageUp => "PageUp".to_owned(),
            KeyCode::PageDown => "PageDown".to_owned(),
            KeyCode::Tab => "Tab".to_owned(),
            other => format!("{other:?}"),
        });
        parts.join("+")
    }
}

pub(crate) fn parse_binding_str(value: &str) -> Result<KeyBinding, crate::config::ConfigError> {
    parse_binding(value)
}

fn parse_binding(value: &str) -> Result<KeyBinding, crate::config::ConfigError> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "enter" => Ok(KeyBinding::new(KeyCode::Enter, KeyModifiers::NONE)),
        "esc" => Ok(KeyBinding::new(KeyCode::Esc, KeyModifiers::NONE)),
        "alt+enter" | "option+enter" => Ok(KeyBinding::new(KeyCode::Enter, KeyModifiers::ALT)),
        "up" => Ok(KeyBinding::new(KeyCode::Up, KeyModifiers::NONE)),
        "down" => Ok(KeyBinding::new(KeyCode::Down, KeyModifiers::NONE)),
        "pageup" | "page_up" | "pgup" => Ok(KeyBinding::new(KeyCode::PageUp, KeyModifiers::NONE)),
        "pagedown" | "page_down" | "pgdown" => {
            Ok(KeyBinding::new(KeyCode::PageDown, KeyModifiers::NONE))
        }
        other => parse_modified_letter(other),
    }
}

fn parse_modified_letter(value: &str) -> Result<KeyBinding, crate::config::ConfigError> {
    let (modifiers, letter) = if let Some(letter) = value.strip_prefix("ctrl+") {
        (KeyModifiers::CONTROL, letter)
    } else if let Some(letter) = value
        .strip_prefix("alt+")
        .or_else(|| value.strip_prefix("option+"))
    {
        (KeyModifiers::ALT, letter)
    } else {
        return Err(crate::config::ConfigError::Invalid(format!(
            "unsupported TUI key binding {value:?}"
        )));
    };
    let mut letters = letter.chars();
    match (letters.next(), letters.next()) {
        (Some(letter), None) if letter.is_ascii_lowercase() => {
            Ok(KeyBinding::new(KeyCode::Char(letter), modifiers))
        }
        _ => Err(crate::config::ConfigError::Invalid(format!(
            "unsupported TUI key binding {value:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TuiKeymapToml;

    #[test]
    fn command_details_defaults_to_alt_t() {
        let keymap = Keymap::default();

        assert_eq!(
            keymap.action_for(KeyBinding::new(KeyCode::Char('t'), KeyModifiers::ALT)),
            Some(KeyAction::OpenCommandDetails)
        );
        assert_eq!(
            keymap.action_for(KeyBinding::new(KeyCode::Char('t'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            keymap.action_for(KeyBinding::new(KeyCode::Char('p'), KeyModifiers::ALT)),
            Some(KeyAction::TogglePlan)
        );
        assert_eq!(
            keymap.action_for(KeyBinding::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            None
        );
        assert_eq!(
            keymap
                .binding_label_for(KeyAction::OpenCommandDetails)
                .as_deref(),
            Some(alt_chord("T").as_str())
        );
    }

    #[test]
    fn macos_option_glyphs_match_the_default_alt_chords() {
        let details =
            command_key_from_option_glyph(KeyEvent::new(KeyCode::Char('†'), KeyModifiers::NONE))
                .expect("option+t on the ABC layout");
        assert_eq!(details.code, KeyCode::Char('t'));
        assert!(details.modifiers.contains(KeyModifiers::ALT));

        assert!(
            command_key_from_option_glyph(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE))
                .is_none()
        );
        assert!(
            command_key_from_option_glyph(KeyEvent::new(KeyCode::Char('†'), KeyModifiers::CONTROL))
                .is_none()
        );
    }

    #[test]
    fn option_prefix_is_the_alt_modifier() {
        let keymap = Keymap::from_config(&TuiKeymapToml {
            open_command_details: Some("option+t".to_owned()),
            insert_newline: Some("option+enter".to_owned()),
            ..TuiKeymapToml::default()
        })
        .expect("option+ is the macOS spelling of an alt chord");

        assert_eq!(
            keymap.action_for(KeyBinding::new(KeyCode::Char('t'), KeyModifiers::ALT)),
            Some(KeyAction::OpenCommandDetails)
        );
        assert_eq!(
            keymap.action_for(KeyBinding::new(KeyCode::Enter, KeyModifiers::ALT)),
            Some(KeyAction::InsertNewline)
        );
        assert_eq!(
            keymap
                .binding_label_for(KeyAction::OpenCommandDetails)
                .as_deref(),
            Some(alt_chord("T").as_str())
        );
    }

    #[test]
    fn explicitly_configured_alt_o_replaces_the_new_default() {
        let keymap = Keymap::from_config(&TuiKeymapToml {
            open_command_details: Some("alt+o".to_owned()),
            ..TuiKeymapToml::default()
        })
        .expect("existing explicit shortcut config should remain valid");

        assert_eq!(
            keymap.action_for(KeyBinding::new(KeyCode::Char('o'), KeyModifiers::ALT)),
            Some(KeyAction::OpenCommandDetails)
        );
        assert_eq!(
            keymap.action_for(KeyBinding::new(KeyCode::Char('t'), KeyModifiers::ALT)),
            None
        );
        assert_eq!(
            keymap
                .binding_label_for(KeyAction::OpenCommandDetails)
                .as_deref(),
            Some(alt_chord("O").as_str())
        );
    }

    #[test]
    fn configured_binding_takes_precedence_over_existing_default_binding() {
        let keymap = Keymap::from_config(&TuiKeymapToml {
            submit: Some("esc".to_owned()),
            ..TuiKeymapToml::default()
        })
        .expect("keymap config should validate");

        assert_eq!(
            keymap.action_for(KeyBinding::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(KeyAction::Submit)
        );
    }

    #[test]
    fn arrow_history_config_keeps_ctrl_p_and_ctrl_n_when_free() {
        let keymap = Keymap::from_config(&TuiKeymapToml {
            history_previous: Some("up".to_owned()),
            history_next: Some("down".to_owned()),
            ..TuiKeymapToml::default()
        })
        .expect("keymap config should validate");

        assert_eq!(
            keymap.action_for(KeyBinding::new(KeyCode::Up, KeyModifiers::NONE)),
            Some(KeyAction::HistoryPrevious)
        );
        assert_eq!(
            keymap.action_for(KeyBinding::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            Some(KeyAction::HistoryPrevious)
        );
        assert_eq!(
            keymap.action_for(KeyBinding::new(KeyCode::Down, KeyModifiers::NONE)),
            Some(KeyAction::HistoryNext)
        );
        assert_eq!(
            keymap.action_for(KeyBinding::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            Some(KeyAction::HistoryNext)
        );
    }

    #[test]
    fn configured_binding_replaces_previous_action_on_same_key() {
        let keymap = Keymap::from_config(&TuiKeymapToml {
            insert_newline: Some("ctrl+r".to_owned()),
            ..TuiKeymapToml::default()
        })
        .expect("keymap config should validate");

        assert_eq!(
            keymap.action_for(KeyBinding::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
            Some(KeyAction::InsertNewline)
        );
    }
}
