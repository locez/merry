use super::ProviderOverlayAction;
use crate::{
    config::{ManagedProviderKind, ProviderAlias, derive_provider_alias},
    provider_management::{ProviderDraft, ProviderManagementError},
    tui::{
        input::{TextInput, TextInputViewport},
        preferences::REASONING_EFFORT_PRESETS,
    },
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_llm::ReasoningEffort;
use merry_provider_openai::OpenAiProtocol;
use std::{collections::BTreeSet, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderFormField {
    DisplayName,
    Alias,
    Kind,
    Protocol,
    BaseUrl,
    ApiKey,
    Model,
    ReasoningEffort,
    Save,
}

impl ProviderFormField {
    pub(crate) const ALL: [Self; 9] = [
        Self::DisplayName,
        Self::Alias,
        Self::Kind,
        Self::Protocol,
        Self::BaseUrl,
        Self::ApiKey,
        Self::Model,
        Self::ReasoningEffort,
        Self::Save,
    ];
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ProviderFormValues {
    pub(crate) display_name: String,
    pub(crate) alias: String,
    pub(crate) kind: ManagedProviderKind,
    pub(crate) protocol: Option<OpenAiProtocol>,
    pub(crate) base_url: String,
    pub(crate) api_key: String,
    pub(crate) model: String,
    pub(crate) reasoning_effort: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderDraftMode {
    Create,
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderFormSeed {
    pub(crate) original_alias: String,
    pub(crate) display_name: String,
    pub(crate) alias: String,
    pub(crate) kind: ManagedProviderKind,
    pub(crate) protocol: Option<OpenAiProtocol>,
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProviderFormMode {
    Add,
    Edit { original_alias: String },
}

impl fmt::Debug for ProviderFormValues {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderFormValues")
            .field("display_name", &self.display_name)
            .field("alias", &self.alias)
            .field("kind", &self.kind)
            .field("protocol", &self.protocol)
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("model", &self.model)
            .field("reasoning_effort", &self.reasoning_effort)
            .finish()
    }
}

impl ProviderFormValues {
    pub(crate) fn to_draft(
        &self,
        mode: ProviderDraftMode,
    ) -> Result<(ProviderAlias, merry_llm::ModelName, ProviderDraft), ProviderManagementError> {
        let alias = ProviderAlias::new(self.alias.trim())
            .map_err(|error| ProviderManagementError::Invalid(error.to_string()))?;
        let model = merry_llm::ModelName::new(self.model.trim())
            .map_err(|error| ProviderManagementError::Invalid(error.to_string()))?;
        let draft = match mode {
            ProviderDraftMode::Update => ProviderDraft::for_update(
                self.display_name.trim(),
                alias.clone(),
                self.kind,
                self.protocol,
                self.base_url.trim(),
                (!self.api_key.trim().is_empty()).then_some(self.api_key.trim()),
                model.clone(),
            )?,
            ProviderDraftMode::Create => ProviderDraft::new(
                self.display_name.trim(),
                alias.clone(),
                self.kind,
                self.protocol,
                self.base_url.trim(),
                &self.api_key,
                model.clone(),
            )?,
        };
        Ok((
            alias,
            model,
            draft.with_reasoning_effort_text(&self.reasoning_effort)?,
        ))
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ProviderFormOverlay {
    fields: [TextInput; 6],
    used_aliases: BTreeSet<String>,
    mode: ProviderFormMode,
    kind: ManagedProviderKind,
    openai_protocol: OpenAiProtocol,
    selected: usize,
    notice: Option<String>,
    alias_edited: bool,
}

impl fmt::Debug for ProviderFormOverlay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderFormOverlay")
            .field("display_name", &self.field(ProviderFormField::DisplayName))
            .field("alias", &self.field(ProviderFormField::Alias))
            .field("kind", &self.kind)
            .field("base_url", &self.field(ProviderFormField::BaseUrl))
            .field("api_key", &"<redacted>")
            .field("model", &self.field(ProviderFormField::Model))
            .field("reasoning_effort", &self.reasoning_effort_text())
            .field("selected", &self.selected)
            .field("notice", &self.notice)
            .finish()
    }
}

impl ProviderFormOverlay {
    pub(crate) fn new(alias: String, used_aliases: BTreeSet<String>) -> Self {
        let mut fields = std::array::from_fn(|_| TextInput::default());
        fields[1].replace_text(alias);
        fields[2].replace_text("https://api.openai.com/v1".to_owned());
        Self {
            fields,
            used_aliases,
            mode: ProviderFormMode::Add,
            kind: ManagedProviderKind::OpenAiCompatible,
            openai_protocol: OpenAiProtocol::Responses,
            selected: 0,
            notice: None,
            alias_edited: false,
        }
    }

    pub(crate) fn edit(seed: ProviderFormSeed, used_aliases: BTreeSet<String>) -> Self {
        let mut fields = std::array::from_fn(|_| TextInput::default());
        fields[0].replace_text(seed.display_name);
        fields[1].replace_text(seed.alias);
        fields[2].replace_text(seed.base_url);
        fields[4].replace_text(seed.model);
        if let Some(reasoning_effort) = seed.reasoning_effort {
            fields[5].replace_text(reasoning_effort.as_str().to_owned());
        }
        Self {
            fields,
            used_aliases,
            mode: ProviderFormMode::Edit {
                original_alias: seed.original_alias,
            },
            kind: seed.kind,
            openai_protocol: seed.protocol.unwrap_or(OpenAiProtocol::Responses),
            selected: 0,
            notice: None,
            alias_edited: true,
        }
    }

    pub(crate) fn title(&self) -> &'static str {
        match self.mode {
            ProviderFormMode::Add => " M  Add provider ",
            ProviderFormMode::Edit { .. } => " M  Edit provider ",
        }
    }

    pub(crate) fn is_editing(&self) -> bool {
        matches!(self.mode, ProviderFormMode::Edit { .. })
    }

    pub(crate) fn selected_field(&self) -> ProviderFormField {
        ProviderFormField::ALL[self.selected.min(ProviderFormField::ALL.len() - 1)]
    }

    pub(crate) fn kind(&self) -> ManagedProviderKind {
        self.kind
    }

    pub(crate) fn protocol(&self) -> Option<OpenAiProtocol> {
        match self.kind {
            ManagedProviderKind::OpenAiCompatible => Some(self.openai_protocol),
            ManagedProviderKind::Anthropic => None,
        }
    }

    pub(crate) fn reasoning_effort_text(&self) -> &str {
        self.field(ProviderFormField::ReasoningEffort)
    }

    #[cfg(test)]
    pub(crate) fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        let value = self.reasoning_effort_text().trim();
        (!value.is_empty())
            .then(|| ReasoningEffort::new(value).ok())
            .flatten()
    }

    pub(crate) fn field(&self, field: ProviderFormField) -> &str {
        match field_input_index(field) {
            Some(index) => self.fields[index].text(),
            None => "",
        }
    }

    pub(crate) fn field_viewport(
        &self,
        field: ProviderFormField,
        width: usize,
    ) -> Option<TextInputViewport> {
        field_input_index(field).map(|index| self.fields[index].viewport(width))
    }

    pub(crate) fn masked_api_key(&self) -> String {
        let masked = "*".repeat(self.field(ProviderFormField::ApiKey).chars().count());
        if masked.is_empty() && self.is_editing() {
            "unchanged".to_owned()
        } else {
            masked
        }
    }

    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub(crate) fn set_model_and_reasoning(&mut self, model: &str, reasoning_effort: &str) {
        self.fields[4].replace_text(model.to_owned());
        self.fields[5].replace_text(reasoning_effort.to_owned());
        self.selected = ProviderFormField::ALL
            .iter()
            .position(|field| *field == ProviderFormField::ReasoningEffort)
            .expect("reasoning effort field is present");
    }

    pub(crate) fn discovery_request(&self) -> (Option<String>, ProviderFormValues) {
        let original_alias = match &self.mode {
            ProviderFormMode::Add => None,
            ProviderFormMode::Edit { original_alias } => Some(original_alias.clone()),
        };
        (original_alias, self.values())
    }

    pub(crate) fn insert_paste(&mut self, text: &str) {
        if self.selected_field() == ProviderFormField::Alias && self.is_editing() {
            self.notice = Some("Config alias is the stable provider ID".to_owned());
            return;
        }
        if let Some(index) = field_input_index(self.selected_field()) {
            self.fields[index].insert_str(text);
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> ProviderOverlayAction {
        self.notice = None;
        match key.code {
            KeyCode::Esc => ProviderOverlayAction::Back,
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.save_action()
            }
            KeyCode::Tab | KeyCode::Down => {
                self.selected = (self.selected + 1) % ProviderFormField::ALL.len();
                ProviderOverlayAction::Consumed
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.selected = (self.selected + ProviderFormField::ALL.len() - 1)
                    % ProviderFormField::ALL.len();
                ProviderOverlayAction::Consumed
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                if self.selected_field() == ProviderFormField::Kind =>
            {
                self.toggle_kind();
                ProviderOverlayAction::Consumed
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                if self.selected_field() == ProviderFormField::Protocol =>
            {
                self.toggle_protocol();
                ProviderOverlayAction::Consumed
            }
            KeyCode::Char(' ')
                if self.selected_field() == ProviderFormField::ReasoningEffort
                    && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.cycle_reasoning();
                ProviderOverlayAction::Consumed
            }
            KeyCode::Enter if self.selected_field() == ProviderFormField::Model => {
                let (original_alias, values) = self.discovery_request();
                ProviderOverlayAction::DiscoverFormModels {
                    original_alias,
                    values,
                }
            }
            KeyCode::Enter if self.selected_field() == ProviderFormField::Save => {
                self.save_action()
            }
            KeyCode::Enter => {
                self.selected = (self.selected + 1).min(ProviderFormField::ALL.len() - 1);
                ProviderOverlayAction::Consumed
            }
            _ => {
                let selected = self.selected_field();
                if let Some(index) = field_input_index(selected) {
                    if selected == ProviderFormField::Alias && self.is_editing() {
                        self.notice = Some("Config alias is the stable provider ID".to_owned());
                        return ProviderOverlayAction::Consumed;
                    }
                    self.fields[index].handle_key(key);
                    if selected == ProviderFormField::Alias {
                        self.alias_edited = true;
                    } else if selected == ProviderFormField::DisplayName
                        && !self.alias_edited
                        && let Ok(alias) =
                            derive_provider_alias(self.fields[index].text(), &self.used_aliases)
                    {
                        self.fields[1].replace_text(alias.as_str().to_owned());
                    }
                }
                ProviderOverlayAction::Consumed
            }
        }
    }

    fn toggle_kind(&mut self) {
        self.kind = match self.kind {
            ManagedProviderKind::OpenAiCompatible => ManagedProviderKind::Anthropic,
            ManagedProviderKind::Anthropic => ManagedProviderKind::OpenAiCompatible,
        };
        let default = match self.kind {
            ManagedProviderKind::OpenAiCompatible => "https://api.openai.com/v1",
            ManagedProviderKind::Anthropic => "https://api.anthropic.com",
        };
        self.fields[2].replace_text(default.to_owned());
    }

    fn toggle_protocol(&mut self) {
        if self.kind != ManagedProviderKind::OpenAiCompatible {
            return;
        }
        self.openai_protocol = match self.openai_protocol {
            OpenAiProtocol::Responses => OpenAiProtocol::ChatCompletions,
            OpenAiProtocol::ChatCompletions => OpenAiProtocol::Responses,
        };
    }

    fn cycle_reasoning(&mut self) {
        let current = self.reasoning_effort_text().trim();
        let next = REASONING_EFFORT_PRESETS
            .iter()
            .position(|value| *value == current)
            .map_or(0, |index| (index + 1) % REASONING_EFFORT_PRESETS.len());
        self.fields[5].replace_text(REASONING_EFFORT_PRESETS[next].to_owned());
    }

    fn save_action(&self) -> ProviderOverlayAction {
        match &self.mode {
            ProviderFormMode::Add => ProviderOverlayAction::SaveProvider(self.values()),
            ProviderFormMode::Edit { original_alias } => ProviderOverlayAction::UpdateProvider {
                original_alias: original_alias.clone(),
                values: self.values(),
            },
        }
    }

    fn values(&self) -> ProviderFormValues {
        ProviderFormValues {
            display_name: self.field(ProviderFormField::DisplayName).to_owned(),
            alias: self.field(ProviderFormField::Alias).to_owned(),
            kind: self.kind,
            protocol: self.protocol(),
            base_url: self.field(ProviderFormField::BaseUrl).to_owned(),
            api_key: self.field(ProviderFormField::ApiKey).to_owned(),
            model: self.field(ProviderFormField::Model).to_owned(),
            reasoning_effort: self.reasoning_effort_text().trim().to_owned(),
        }
    }
}

fn field_input_index(field: ProviderFormField) -> Option<usize> {
    match field {
        ProviderFormField::DisplayName => Some(0),
        ProviderFormField::Alias => Some(1),
        ProviderFormField::Kind => None,
        ProviderFormField::Protocol => None,
        ProviderFormField::BaseUrl => Some(2),
        ProviderFormField::ApiKey => Some(3),
        ProviderFormField::Model => Some(4),
        ProviderFormField::ReasoningEffort => Some(5),
        ProviderFormField::Save => None,
    }
}
