use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)]
pub(crate) enum KeyAction {
    SubmitNext,
    SubmitBacklog,
    CancelInputOrQuit,
    InsertNewline,
    Interrupt,
    OpenCommandPanel,
    OpenDetails,
    CloseOverlay,
    Quit,
    ScrollUp,
    ScrollDown,
    ReviewPreviousUserInput,
    HistoryPrevious,
    HistoryNext,
    ResumeSuspended,
    DiscardSuspended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)]
pub(crate) struct KeyBinding {
    code: KeyCode,
    modifiers: KeyModifiers,
}

#[allow(dead_code)]
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
#[allow(dead_code)]
pub(crate) struct Keymap {
    bindings: Vec<(KeyBinding, KeyAction)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            bindings: vec![
                (
                    KeyBinding::new(KeyCode::Enter, KeyModifiers::NONE),
                    KeyAction::SubmitNext,
                ),
                (
                    KeyBinding::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
                    KeyAction::SubmitBacklog,
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
                    KeyBinding::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
                    KeyAction::OpenCommandPanel,
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
                    KeyBinding::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                    KeyAction::ReviewPreviousUserInput,
                ),
                (
                    KeyBinding::new(KeyCode::Up, KeyModifiers::NONE),
                    KeyAction::HistoryPrevious,
                ),
                (
                    KeyBinding::new(KeyCode::Down, KeyModifiers::NONE),
                    KeyAction::HistoryNext,
                ),
            ],
        }
    }
}

#[allow(dead_code)]
impl Keymap {
    pub(crate) fn from_config(
        config: &crate::config::TuiKeymapToml,
    ) -> Result<Self, crate::config::ConfigError> {
        let mut keymap = Self::default();
        if let Some(binding) = config.submit_next.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::SubmitNext);
        }
        if let Some(binding) = config.submit_backlog.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::SubmitBacklog);
        }
        if let Some(binding) = config.cancel_input_or_quit.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::CancelInputOrQuit);
        }
        if let Some(binding) = config.insert_newline.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::InsertNewline);
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
        if let Some(binding) = config.review_previous_user_input.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::ReviewPreviousUserInput);
        }
        if let Some(binding) = config.history_previous.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::HistoryPrevious);
        }
        if let Some(binding) = config.history_next.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::HistoryNext);
        }
        if let Some(binding) = config.resume_suspended.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::ResumeSuspended);
        }
        if let Some(binding) = config.discard_suspended.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::DiscardSuspended);
        }
        Ok(keymap)
    }

    pub(crate) fn action_for(&self, binding: KeyBinding) -> Option<KeyAction> {
        self.bindings
            .iter()
            .rev()
            .find_map(|(candidate, action)| (*candidate == binding).then_some(*action))
    }

    fn set_binding(&mut self, binding: KeyBinding, action: KeyAction) {
        self.bindings
            .retain(|(candidate_binding, candidate_action)| {
                *candidate_action != action && *candidate_binding != binding
            });
        self.bindings.push((binding, action));
    }
}

fn parse_binding(value: &str) -> Result<KeyBinding, crate::config::ConfigError> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "enter" => Ok(KeyBinding::new(KeyCode::Enter, KeyModifiers::NONE)),
        "esc" => Ok(KeyBinding::new(KeyCode::Esc, KeyModifiers::NONE)),
        "up" => Ok(KeyBinding::new(KeyCode::Up, KeyModifiers::NONE)),
        "down" => Ok(KeyBinding::new(KeyCode::Down, KeyModifiers::NONE)),
        "pageup" | "page_up" | "pgup" => Ok(KeyBinding::new(KeyCode::PageUp, KeyModifiers::NONE)),
        "pagedown" | "page_down" | "pgdown" => {
            Ok(KeyBinding::new(KeyCode::PageDown, KeyModifiers::NONE))
        }
        "ctrl+b" => Ok(KeyBinding::new(KeyCode::Char('b'), KeyModifiers::CONTROL)),
        "ctrl+c" => Ok(KeyBinding::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        "ctrl+d" => Ok(KeyBinding::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
        "ctrl+j" => Ok(KeyBinding::new(KeyCode::Char('j'), KeyModifiers::CONTROL)),
        "ctrl+n" => Ok(KeyBinding::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
        "ctrl+p" => Ok(KeyBinding::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
        "ctrl+q" => Ok(KeyBinding::new(KeyCode::Char('q'), KeyModifiers::CONTROL)),
        "ctrl+r" => Ok(KeyBinding::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
        "ctrl+u" => Ok(KeyBinding::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
        other => Err(crate::config::ConfigError::Invalid(format!(
            "unsupported TUI key binding {other:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TuiKeymapToml;

    #[test]
    fn configured_binding_takes_precedence_over_existing_default_binding() {
        let keymap = Keymap::from_config(&TuiKeymapToml {
            submit_next: Some("esc".to_owned()),
            ..TuiKeymapToml::default()
        })
        .expect("keymap config should validate");

        assert_eq!(
            keymap.action_for(KeyBinding::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(KeyAction::SubmitNext)
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
