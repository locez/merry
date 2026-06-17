use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)]
pub(crate) enum KeyAction {
    SubmitNext,
    SubmitBacklog,
    Interrupt,
    OpenCommandPanel,
    OpenDetails,
    CloseOverlay,
    Quit,
    ScrollUp,
    ScrollDown,
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
                    KeyAction::Quit,
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
                    KeyBinding::new(KeyCode::Up, KeyModifiers::NONE),
                    KeyAction::ScrollUp,
                ),
                (
                    KeyBinding::new(KeyCode::Down, KeyModifiers::NONE),
                    KeyAction::ScrollDown,
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
        if let Some(binding) = config.interrupt.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::Interrupt);
        }
        if let Some(binding) = config.quit.as_deref() {
            keymap.set_binding(parse_binding(binding)?, KeyAction::Quit);
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
            .retain(|(_, candidate_action)| *candidate_action != action);
        self.bindings.push((binding, action));
    }
}

fn parse_binding(value: &str) -> Result<KeyBinding, crate::config::ConfigError> {
    match value {
        "enter" => Ok(KeyBinding::new(KeyCode::Enter, KeyModifiers::NONE)),
        "esc" => Ok(KeyBinding::new(KeyCode::Esc, KeyModifiers::NONE)),
        "ctrl+b" => Ok(KeyBinding::new(KeyCode::Char('b'), KeyModifiers::CONTROL)),
        "ctrl+c" => Ok(KeyBinding::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        "ctrl+q" => Ok(KeyBinding::new(KeyCode::Char('q'), KeyModifiers::CONTROL)),
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
}
