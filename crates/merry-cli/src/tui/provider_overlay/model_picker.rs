use super::{ModelPickerTarget, ProviderOverlayAction};
use crate::tui::input::{TextInput, TextInputViewport};
use crossterm::event::{KeyCode, KeyEvent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelListItem {
    id: String,
    owner: Option<String>,
}

impl ModelListItem {
    pub(crate) fn new(id: &str, owner: Option<&str>) -> Self {
        Self {
            id: id.to_owned(),
            owner: owner.map(str::to_owned),
        }
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelPickerOverlay {
    alias: String,
    display_name: String,
    models: Vec<ModelListItem>,
    query: TextInput,
    selected: usize,
    loading: bool,
    error: Option<String>,
    target: ModelPickerTarget,
}

impl ModelPickerOverlay {
    pub(crate) fn new(
        alias: String,
        display_name: String,
        models: Vec<ModelListItem>,
        loading: bool,
    ) -> Self {
        Self::with_target(
            alias,
            display_name,
            models,
            loading,
            ModelPickerTarget::ActiveProvider,
        )
    }

    pub(crate) fn for_provider_form(
        alias: String,
        display_name: String,
        models: Vec<ModelListItem>,
    ) -> Self {
        Self::with_target(
            alias,
            display_name,
            models,
            true,
            ModelPickerTarget::ProviderForm,
        )
    }

    fn with_target(
        alias: String,
        display_name: String,
        models: Vec<ModelListItem>,
        loading: bool,
        target: ModelPickerTarget,
    ) -> Self {
        Self {
            alias,
            display_name,
            models,
            query: TextInput::default(),
            selected: 0,
            loading,
            error: None,
            target,
        }
    }

    pub(crate) fn alias(&self) -> &str {
        &self.alias
    }

    pub(crate) fn display_name(&self) -> &str {
        &self.display_name
    }

    pub(crate) fn target(&self) -> ModelPickerTarget {
        self.target
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

    pub(crate) fn is_loading(&self) -> bool {
        self.loading
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub(crate) fn visible_models(&self) -> Vec<&ModelListItem> {
        let query = self.query.text().trim().to_ascii_lowercase();
        self.models
            .iter()
            .filter(|model| {
                query.is_empty()
                    || model.id.to_ascii_lowercase().contains(&query)
                    || model
                        .owner
                        .as_ref()
                        .is_some_and(|owner| owner.to_ascii_lowercase().contains(&query))
            })
            .collect()
    }

    pub(crate) fn manual_model(&self) -> Option<&str> {
        let query = self.query.text().trim();
        (!query.is_empty()
            && merry_llm::ModelName::new(query).is_ok()
            && !self.models.iter().any(|model| model.id == query))
        .then_some(query)
    }

    pub(crate) fn set_models(&mut self, models: Vec<ModelListItem>) {
        self.models = models;
        self.loading = false;
        self.error = None;
        self.selected = self
            .selected
            .min(self.visible_models().len().saturating_sub(1));
    }

    pub(crate) fn set_loading(&mut self) {
        self.loading = true;
        self.error = None;
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> ProviderOverlayAction {
        match key.code {
            KeyCode::Esc => ProviderOverlayAction::Back,
            KeyCode::Down => {
                self.selected =
                    (self.selected + 1).min(self.visible_models().len().saturating_sub(1));
                ProviderOverlayAction::Consumed
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                ProviderOverlayAction::Consumed
            }
            KeyCode::F(5) => match self.target {
                ModelPickerTarget::ActiveProvider => {
                    ProviderOverlayAction::RefreshModels(self.alias.clone())
                }
                ModelPickerTarget::ProviderForm => ProviderOverlayAction::RefreshFormModels,
            },
            KeyCode::Enter => {
                if let Some(model) = self.visible_models().get(self.selected) {
                    ProviderOverlayAction::SelectModel {
                        alias: self.alias.clone(),
                        model: model.id.clone(),
                        target: self.target,
                    }
                } else if let Some(model) = self.manual_model() {
                    ProviderOverlayAction::SelectModel {
                        alias: self.alias.clone(),
                        model: model.to_owned(),
                        target: self.target,
                    }
                } else {
                    ProviderOverlayAction::Consumed
                }
            }
            _ => {
                self.query.handle_key(key);
                self.selected = 0;
                ProviderOverlayAction::Consumed
            }
        }
    }
}
