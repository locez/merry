use super::{
    input::{TextInput, TextInputViewport},
    preferences::REASONING_EFFORT_PRESETS,
    provider_selection::ModelPickerTarget,
};
use crossterm::event::{KeyCode, KeyEvent};
use merry::providers::ReasoningEffort;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReasoningPickerOverlay {
    alias: String,
    model: String,
    target: ModelPickerTarget,
    query: TextInput,
    selected: usize,
    error: Option<String>,
}

impl ReasoningPickerOverlay {
    pub(crate) fn new(alias: String, model: String, target: ModelPickerTarget) -> Self {
        Self {
            alias,
            model,
            target,
            query: TextInput::default(),
            selected: 0,
            error: None,
        }
    }

    pub(crate) fn with_initial_value(mut self, initial: Option<&str>) -> Self {
        if let Some(initial) = initial.map(str::trim).filter(|value| !value.is_empty()) {
            if let Some(index) = REASONING_EFFORT_PRESETS
                .iter()
                .position(|preset| *preset == initial)
            {
                self.selected = index;
            } else {
                self.query.replace_text(initial.to_owned());
                self.selected = 0;
            }
        }
        self
    }

    pub(crate) fn alias(&self) -> &str {
        &self.alias
    }

    pub(crate) fn model(&self) -> &str {
        &self.model
    }

    pub(crate) fn query(&self) -> &str {
        self.query.text()
    }

    pub(crate) fn query_viewport(&self, width: usize) -> TextInputViewport {
        self.query.viewport(width)
    }

    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    pub(crate) fn select(&mut self, index: usize) {
        let count = self.option_count();
        if index < count {
            self.selected = index;
        }
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub(crate) fn insert_paste(&mut self, text: &str) {
        let single_line = crate::text::single_line_paste_collapsed(text);
        self.query.insert_str(&single_line);
        self.selected = 0;
        self.error = None;
    }

    pub(crate) fn visible_presets(&self) -> Vec<&'static str> {
        let query = self.query.text().trim().to_ascii_lowercase();
        REASONING_EFFORT_PRESETS
            .iter()
            .copied()
            .filter(|preset| query.is_empty() || preset.contains(&query))
            .collect()
    }

    pub(crate) fn custom_candidate(&self) -> Option<&str> {
        let query = self.query.text().trim();
        (!query.is_empty() && !REASONING_EFFORT_PRESETS.contains(&query)).then_some(query)
    }

    pub(crate) fn option_count(&self) -> usize {
        self.visible_presets().len() + usize::from(self.custom_candidate().is_some())
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> ReasoningPickerAction {
        match key.code {
            KeyCode::Esc => ReasoningPickerAction::Back,
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.option_count().saturating_sub(1));
                ReasoningPickerAction::Consumed
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                ReasoningPickerAction::Consumed
            }
            KeyCode::Enter => {
                let presets = self.visible_presets();
                if let Some(value) = presets.get(self.selected) {
                    let reasoning_effort = ReasoningEffort::new(value)
                        .expect("built-in reasoning effort should validate");
                    ReasoningPickerAction::SelectReasoning {
                        alias: self.alias.clone(),
                        model: self.model.clone(),
                        reasoning_effort,
                        target: self.target.clone(),
                    }
                } else if let Some(custom) = self.custom_candidate() {
                    match ReasoningEffort::new(custom) {
                        Ok(reasoning_effort) => ReasoningPickerAction::SelectReasoning {
                            alias: self.alias.clone(),
                            model: self.model.clone(),
                            reasoning_effort,
                            target: self.target.clone(),
                        },
                        Err(error) => {
                            self.error = Some(error.to_string());
                            ReasoningPickerAction::Consumed
                        }
                    }
                } else {
                    ReasoningPickerAction::Consumed
                }
            }
            _ => {
                self.query.handle_key(key);
                self.selected = 0;
                self.error = None;
                ReasoningPickerAction::Consumed
            }
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
        assert_eq!(picker.option_count(), REASONING_EFFORT_PRESETS.len());
        assert!(
            picker
                .visible_presets()
                .iter()
                .all(|preset| *preset != "Default")
        );

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
        custom.insert_paste("max ultra");
        assert_eq!(custom.custom_candidate(), Some("max ultra"));
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

    #[test]
    fn reasoning_picker_filters_presets_and_keeps_custom_value_selectable() {
        let mut picker = ReasoningPickerOverlay::new(
            "opencode".to_owned(),
            "model-a".to_owned(),
            ModelPickerTarget::ActiveProvider,
        );
        picker.insert_paste("hi");
        assert_eq!(picker.visible_presets(), vec!["high", "xhigh"]);
        assert_eq!(picker.custom_candidate(), Some("hi"));
        assert_eq!(picker.option_count(), 3);

        // Down twice selects the custom candidate "hi".
        let _ = picker.handle_key(KeyEvent::new(
            KeyCode::Down,
            crossterm::event::KeyModifiers::NONE,
        ));
        let _ = picker.handle_key(KeyEvent::new(
            KeyCode::Down,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(
            picker.handle_key(KeyEvent::new(
                KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            )),
            ReasoningPickerAction::SelectReasoning {
                alias: "opencode".to_owned(),
                model: "model-a".to_owned(),
                reasoning_effort: ReasoningEffort::new("hi").expect("custom is valid"),
                target: ModelPickerTarget::ActiveProvider,
            }
        );
    }
}
