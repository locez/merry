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
use merry::providers::OpenAiProtocol;
use merry::providers::{ReasoningEffort, ServiceTier};
use std::{collections::BTreeSet, fmt};

const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";

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
    ServiceTier,
    Save,
}

impl ProviderFormField {
    pub(crate) const ALL: [Self; 10] = [
        Self::DisplayName,
        Self::Alias,
        Self::Kind,
        Self::Protocol,
        Self::BaseUrl,
        Self::ApiKey,
        Self::Model,
        Self::ReasoningEffort,
        Self::ServiceTier,
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
    pub(crate) service_tier: Option<ServiceTier>,
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
    pub(crate) service_tier: Option<ServiceTier>,
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
            .field("service_tier", &self.service_tier)
            .finish()
    }
}

impl ProviderFormValues {
    pub(crate) fn to_draft(
        &self,
        mode: ProviderDraftMode,
    ) -> Result<(ProviderAlias, merry::providers::ModelName, ProviderDraft), ProviderManagementError>
    {
        let alias = ProviderAlias::new(self.alias.trim())
            .map_err(|error| ProviderManagementError::Invalid(error.to_string()))?;
        let model = merry::providers::ModelName::new(self.model.trim())
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
        let draft = draft
            .with_reasoning_effort_text(&self.reasoning_effort)?
            .with_service_tier(self.service_tier)?;
        Ok((alias, model, draft))
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ProviderFormOverlay {
    fields: [TextInput; 6],
    used_aliases: BTreeSet<String>,
    mode: ProviderFormMode,
    kind: ManagedProviderKind,
    openai_protocol: OpenAiProtocol,
    openai_service_tier: Option<ServiceTier>,
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
            .field("protocol", &self.protocol())
            .field("base_url", &self.field(ProviderFormField::BaseUrl))
            .field("api_key", &"<redacted>")
            .field("model", &self.field(ProviderFormField::Model))
            .field("reasoning_effort", &self.reasoning_effort_text())
            .field("service_tier", &self.service_tier())
            .field("selected", &self.selected)
            .field("notice", &self.notice)
            .finish()
    }
}

impl ProviderFormOverlay {
    pub(crate) fn new(alias: String, used_aliases: BTreeSet<String>) -> Self {
        let mut fields = std::array::from_fn(|_| TextInput::default());
        fields[1].replace_text(alias);
        fields[2].replace_text(DEFAULT_OPENAI_BASE_URL.to_owned());
        Self {
            fields,
            used_aliases,
            mode: ProviderFormMode::Add,
            kind: ManagedProviderKind::OpenAiCompatible,
            openai_protocol: OpenAiProtocol::Responses,
            openai_service_tier: None,
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
            openai_service_tier: seed.service_tier,
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

    pub(crate) fn editing_alias(&self) -> Option<&str> {
        match &self.mode {
            ProviderFormMode::Edit { original_alias } => Some(original_alias.as_str()),
            ProviderFormMode::Add => None,
        }
    }

    pub(crate) fn selected_field(&self) -> ProviderFormField {
        ProviderFormField::ALL[self.selected.min(ProviderFormField::ALL.len() - 1)]
    }

    pub(crate) fn select_field(&mut self, field: ProviderFormField) {
        if let Some(index) = ProviderFormField::ALL
            .iter()
            .position(|candidate| *candidate == field)
        {
            self.selected = index;
        }
    }

    pub(crate) fn click_field(&mut self, field: ProviderFormField) -> ProviderOverlayAction {
        self.notice = None;
        self.select_field(field);
        match field {
            ProviderFormField::Kind => {
                self.toggle_kind();
                ProviderOverlayAction::Consumed
            }
            ProviderFormField::Protocol => {
                self.toggle_protocol();
                ProviderOverlayAction::Consumed
            }
            ProviderFormField::ServiceTier => {
                self.cycle_service_tier(1);
                ProviderOverlayAction::Consumed
            }
            ProviderFormField::Save => self.save_action(),
            ProviderFormField::Alias if self.is_editing() => {
                self.notice = Some("Config alias is the stable provider ID".to_owned());
                ProviderOverlayAction::Consumed
            }
            _ => ProviderOverlayAction::Consumed,
        }
    }

    pub(crate) fn trigger_model_discovery(&self) -> ProviderOverlayAction {
        let (original_alias, values) = self.discovery_request();
        ProviderOverlayAction::DiscoverFormModels {
            original_alias,
            values,
        }
    }

    pub(crate) fn trigger_copy_api_key(&mut self) -> ProviderOverlayAction {
        self.select_field(ProviderFormField::ApiKey);
        if !self.has_copyable_api_key() {
            self.notice = Some("No API key to copy".to_owned());
            return ProviderOverlayAction::Consumed;
        }
        ProviderOverlayAction::CopyApiKey
    }

    pub(crate) fn has_copyable_api_key(&self) -> bool {
        self.is_editing() || !self.field(ProviderFormField::ApiKey).trim().is_empty()
    }

    pub(crate) fn api_key_copy_source(&self) -> (Option<String>, Option<String>) {
        let draft = self.field(ProviderFormField::ApiKey).trim();
        let draft_api_key = (!draft.is_empty()).then(|| draft.to_owned());
        (self.editing_alias().map(str::to_owned), draft_api_key)
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

    pub(crate) fn service_tier(&self) -> Option<ServiceTier> {
        match self.kind {
            ManagedProviderKind::OpenAiCompatible => self.openai_service_tier,
            ManagedProviderKind::Anthropic => None,
        }
    }

    pub(crate) fn service_tier_label(&self) -> String {
        match self.kind {
            ManagedProviderKind::Anthropic => "N/A (OpenAI-compatible only)".to_owned(),
            ManagedProviderKind::OpenAiCompatible => match self.openai_service_tier {
                None => "Unset".to_owned(),
                Some(tier) if !self.openai_protocol.supports_service_tier(tier) => {
                    format!("{} (requires Responses)", tier.as_str())
                }
                Some(tier) => tier.as_str().to_owned(),
            },
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
        let len = self.field(ProviderFormField::ApiKey).chars().count();
        if len > 0 {
            "*".repeat(len)
        } else if self.is_editing() {
            "•••••••••••• (saved)".to_owned()
        } else {
            String::new()
        }
    }

    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub(crate) fn set_notice(&mut self, notice: Option<String>) {
        self.notice = notice;
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
            let single_line = crate::text::single_line_paste_stripped(text);
            self.fields[index].insert_str(&single_line);
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> ProviderOverlayAction {
        self.notice = None;
        match key.code {
            KeyCode::Esc => ProviderOverlayAction::Back,
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.save_action()
            }
            KeyCode::Char('y' | 'Y')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.selected_field() == ProviderFormField::ApiKey =>
            {
                self.trigger_copy_api_key()
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
            KeyCode::Left if self.selected_field() == ProviderFormField::ServiceTier => {
                self.cycle_service_tier(-1);
                ProviderOverlayAction::Consumed
            }
            KeyCode::Right | KeyCode::Char(' ')
                if self.selected_field() == ProviderFormField::ServiceTier =>
            {
                self.cycle_service_tier(1);
                ProviderOverlayAction::Consumed
            }
            KeyCode::Backspace | KeyCode::Delete
                if self.selected_field() == ProviderFormField::ServiceTier =>
            {
                if self.kind == ManagedProviderKind::OpenAiCompatible {
                    self.openai_service_tier = None;
                }
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
        let old_default = default_base_url(self.kind);
        self.kind = match self.kind {
            ManagedProviderKind::OpenAiCompatible => ManagedProviderKind::Anthropic,
            ManagedProviderKind::Anthropic => ManagedProviderKind::OpenAiCompatible,
        };
        let current_url = self.fields[2].text().trim();
        if current_url.is_empty() || current_url == old_default {
            self.fields[2].replace_text(default_base_url(self.kind).to_owned());
        }
    }

    fn toggle_protocol(&mut self) {
        if self.kind != ManagedProviderKind::OpenAiCompatible {
            return;
        }
        self.openai_protocol = match self.openai_protocol {
            OpenAiProtocol::Responses => OpenAiProtocol::ChatCompletions,
            OpenAiProtocol::ChatCompletions => OpenAiProtocol::Responses,
        };
        if let Some(tier) = self.openai_service_tier
            && !self.openai_protocol.supports_service_tier(tier)
        {
            self.notice = Some(format!(
                "Service tier \"{tier}\" requires Responses; change tier or switch back before saving"
            ));
        }
    }

    fn cycle_service_tier(&mut self, delta: isize) {
        if self.kind != ManagedProviderKind::OpenAiCompatible {
            self.notice =
                Some("Service tier is only supported for OpenAI-compatible providers".to_owned());
            return;
        }
        let mut choices: Vec<Option<ServiceTier>> = vec![None];
        for tier in ServiceTier::ALL {
            if self.openai_protocol.supports_service_tier(tier) {
                choices.push(Some(tier));
            }
        }
        let current = choices
            .iter()
            .position(|choice| *choice == self.openai_service_tier)
            .unwrap_or(0);
        let len = choices.len();
        let next = if delta < 0 {
            (current + len - 1) % len
        } else {
            (current + 1) % len
        };
        self.openai_service_tier = choices[next];
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
            service_tier: self.service_tier(),
        }
    }
}

fn default_base_url(kind: ManagedProviderKind) -> &'static str {
    match kind {
        ManagedProviderKind::OpenAiCompatible => DEFAULT_OPENAI_BASE_URL,
        ManagedProviderKind::Anthropic => DEFAULT_ANTHROPIC_BASE_URL,
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
        ProviderFormField::ServiceTier => None,
        ProviderFormField::Save => None,
    }
}
