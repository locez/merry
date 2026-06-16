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
                    KeyAction::Interrupt,
                ),
                (
                    KeyBinding::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
                    KeyAction::OpenCommandPanel,
                ),
                (
                    KeyBinding::new(KeyCode::Esc, KeyModifiers::NONE),
                    KeyAction::CloseOverlay,
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
    pub(crate) fn action_for(&self, binding: KeyBinding) -> Option<KeyAction> {
        self.bindings
            .iter()
            .find_map(|(candidate, action)| (*candidate == binding).then_some(*action))
    }
}
