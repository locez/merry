use super::{
    input::TextInput, preferences::REASONING_EFFORT_PRESETS, provider_selection::ModelPickerTarget,
};
use crossterm::event::{KeyCode, KeyEvent};
use merry_llm::ReasoningEffort;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReasoningPickerOverlay {
    alias: String,
    model: String,
    target: ModelPickerTarget,
    selected: usize,
    custom_editor: Option<TextInput>,
    error: Option<String>,
}

impl ReasoningPickerOverlay {
    pub(crate) fn new(alias: String, model: String, target: ModelPickerTarget) -> Self {
        Self {
            alias,
            model,
            target,
            selected: 0,
            custom_editor: None,
            error: None,
        }
    }

    pub(crate) fn alias(&self) -> &str {
        &self.alias
    }

    pub(crate) fn model(&self) -> &str {
        &self.model
    }

    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    pub(crate) fn custom_editor(&self) -> Option<&TextInput> {
        self.custom_editor.as_ref()
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub(crate) fn insert_paste(&mut self, text: &str) {
        if let Some(editor) = self.custom_editor.as_mut() {
            editor.insert_str(text);
            self.error = None;
        }
    }

    pub(crate) fn option_count(&self) -> usize {
        REASONING_EFFORT_PRESETS.len() + 1
    }

    pub(crate) fn option_label(&self, index: usize) -> &str {
        REASONING_EFFORT_PRESETS
            .get(index)
            .copied()
            .unwrap_or("Custom")
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> ReasoningPickerAction {
        if let Some(editor) = self.custom_editor.as_mut() {
            return match key.code {
                KeyCode::Esc => {
                    self.custom_editor = None;
                    self.error = None;
                    ReasoningPickerAction::Consumed
                }
                KeyCode::Enter => match ReasoningEffort::new(editor.text().trim()) {
                    Ok(reasoning_effort) => ReasoningPickerAction::SelectReasoning {
                        alias: self.alias.clone(),
                        model: self.model.clone(),
                        reasoning_effort,
                        target: self.target,
                    },
                    Err(error) => {
                        self.error = Some(error.to_string());
                        ReasoningPickerAction::Consumed
                    }
                },
                _ => {
                    editor.handle_key(key);
                    self.error = None;
                    ReasoningPickerAction::Consumed
                }
            };
        }

        match key.code {
            KeyCode::Esc => ReasoningPickerAction::Back,
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.option_count() - 1);
                ReasoningPickerAction::Consumed
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                ReasoningPickerAction::Consumed
            }
            KeyCode::Enter => {
                if let Some(value) = REASONING_EFFORT_PRESETS.get(self.selected) {
                    let reasoning_effort = ReasoningEffort::new(value)
                        .expect("built-in reasoning effort should validate");
                    ReasoningPickerAction::SelectReasoning {
                        alias: self.alias.clone(),
                        model: self.model.clone(),
                        reasoning_effort,
                        target: self.target,
                    }
                } else {
                    self.custom_editor = Some(TextInput::default());
                    self.error = None;
                    ReasoningPickerAction::Consumed
                }
            }
            _ => ReasoningPickerAction::Consumed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReasoningPickerAction {
    Consumed,
    Back,
    SelectReasoning {
        alias: String,
        model: String,
        reasoning_effort: ReasoningEffort,
        target: ModelPickerTarget,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_picker_has_no_empty_default_and_accepts_custom_values() {
        let mut picker = ReasoningPickerOverlay::new(
            "opencode".to_owned(),
            "model-a".to_owned(),
            ModelPickerTarget::ActiveProvider,
        );
        assert_eq!(picker.option_count(), REASONING_EFFORT_PRESETS.len() + 1);
        assert!((0..picker.option_count()).all(|index| picker.option_label(index) != "Default"));

        assert_eq!(
            picker.handle_key(KeyEvent::new(
                KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE
            )),
            ReasoningPickerAction::SelectReasoning {
                alias: "opencode".to_owned(),
                model: "model-a".to_owned(),
                reasoning_effort: ReasoningEffort::new("minimal").expect("preset is valid"),
                target: ModelPickerTarget::ActiveProvider,
            }
        );

        let mut custom = ReasoningPickerOverlay::new(
            "opencode".to_owned(),
            "model-b".to_owned(),
            ModelPickerTarget::ActiveProvider,
        );
        for _ in 0..REASONING_EFFORT_PRESETS.len() {
            let _ = custom.handle_key(KeyEvent::new(
                KeyCode::Down,
                crossterm::event::KeyModifiers::NONE,
            ));
        }
        assert_eq!(
            custom.handle_key(KeyEvent::new(
                KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            )),
            ReasoningPickerAction::Consumed
        );
        custom
            .custom_editor
            .as_mut()
            .expect("custom editor should open")
            .replace_text("max ultra".to_owned());
        assert_eq!(
            custom.handle_key(KeyEvent::new(
                KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            )),
            ReasoningPickerAction::SelectReasoning {
                alias: "opencode".to_owned(),
                model: "model-b".to_owned(),
                reasoning_effort: ReasoningEffort::new("max ultra").expect("custom is valid"),
                target: ModelPickerTarget::ActiveProvider,
            }
        );
    }
}
