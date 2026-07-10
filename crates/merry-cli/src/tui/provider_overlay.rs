use super::input::{TextInput, TextInputViewport};
use crate::config::{
    ConfiguredProviderKind, ManagedProviderKind, ProviderConfigSource, derive_provider_alias,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_provider_openai::OpenAiProtocol;
use std::{collections::BTreeSet, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderListItem {
    alias: String,
    display_name: String,
    kind: ConfiguredProviderKind,
    source: ProviderConfigSource,
    protocol: Option<OpenAiProtocol>,
    model: Option<String>,
}

impl ProviderListItem {
    pub(crate) fn new(
        alias: &str,
        display_name: &str,
        kind: ConfiguredProviderKind,
        source: ProviderConfigSource,
        protocol: Option<OpenAiProtocol>,
        model: Option<&str>,
    ) -> Self {
        Self {
            alias: alias.to_owned(),
            display_name: display_name.to_owned(),
            kind,
            source,
            protocol,
            model: model.map(str::to_owned),
        }
    }

    pub(crate) fn alias(&self) -> &str {
        &self.alias
    }

    pub(crate) fn display_name(&self) -> &str {
        &self.display_name
    }

    pub(crate) fn kind(&self) -> ConfiguredProviderKind {
        self.kind
    }

    pub(crate) fn source(&self) -> ProviderConfigSource {
        self.source
    }

    pub(crate) fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub(crate) fn protocol(&self) -> Option<OpenAiProtocol> {
        self.protocol
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderManagerOverlay {
    items: Vec<ProviderListItem>,
    selected: usize,
    current_alias: Option<String>,
    notice: Option<String>,
    pending_delete_alias: Option<String>,
}

impl ProviderManagerOverlay {
    pub(crate) fn new(items: Vec<ProviderListItem>, current_alias: Option<&str>) -> Self {
        let selected = current_alias
            .and_then(|alias| items.iter().position(|item| item.alias() == alias))
            .unwrap_or(0);
        Self {
            items,
            selected,
            current_alias: current_alias.map(str::to_owned),
            notice: None,
            pending_delete_alias: None,
        }
    }

    pub(crate) fn items(&self) -> &[ProviderListItem] {
        &self.items
    }

    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    pub(crate) fn current_alias(&self) -> Option<&str> {
        self.current_alias.as_deref()
    }

    pub(crate) fn selected_source(&self) -> Option<ProviderConfigSource> {
        self.items.get(self.selected).map(ProviderListItem::source)
    }

    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> ProviderOverlayAction {
        match key.code {
            KeyCode::Esc => ProviderOverlayAction::Back,
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                self.pending_delete_alias = None;
                ProviderOverlayAction::Consumed
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.items.len().saturating_sub(1));
                self.pending_delete_alias = None;
                ProviderOverlayAction::Consumed
            }
            KeyCode::Enter => self
                .items
                .get(self.selected)
                .map_or(ProviderOverlayAction::OpenProviderForm, |item| {
                    ProviderOverlayAction::OpenProviderEditor(item.alias.clone())
                }),
            KeyCode::Char('m') => self
                .items
                .get(self.selected)
                .map_or(ProviderOverlayAction::Consumed, |item| {
                    ProviderOverlayAction::OpenModelPicker(item.alias.clone())
                }),
            KeyCode::Char('n') => ProviderOverlayAction::OpenProviderForm,
            KeyCode::Char('d') => {
                let Some(item) = self.items.get(self.selected) else {
                    return ProviderOverlayAction::Consumed;
                };
                if item.source() == ProviderConfigSource::User {
                    self.notice = Some("User config providers are read-only".to_owned());
                    return ProviderOverlayAction::Consumed;
                }
                if self.current_alias() == Some(item.alias()) {
                    self.notice = Some("Switch provider before deleting the active one".to_owned());
                    return ProviderOverlayAction::Consumed;
                }
                if self.pending_delete_alias.as_deref() == Some(item.alias()) {
                    ProviderOverlayAction::DeleteProvider(item.alias.clone())
                } else {
                    self.pending_delete_alias = Some(item.alias.clone());
                    self.notice = Some(format!("Press d again to delete {}", item.display_name()));
                    ProviderOverlayAction::Consumed
                }
            }
            _ => ProviderOverlayAction::Consumed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderFormField {
    DisplayName,
    Alias,
    Kind,
    Protocol,
    BaseUrl,
    ApiKey,
    Model,
    Save,
}

impl ProviderFormField {
    pub(crate) const ALL: [Self; 8] = [
        Self::DisplayName,
        Self::Alias,
        Self::Kind,
        Self::Protocol,
        Self::BaseUrl,
        Self::ApiKey,
        Self::Model,
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
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ProviderFormOverlay {
    fields: [TextInput; 5],
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

    pub(crate) fn set_model(&mut self, model: &str) {
        self.fields[4].replace_text(model.to_owned());
        self.selected = ProviderFormField::ALL
            .iter()
            .position(|field| *field == ProviderFormField::Model)
            .expect("model field is present");
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
        }
    }

    #[cfg(test)]
    fn set_field_for_test(&mut self, field: ProviderFormField, value: &str) {
        let index = field_input_index(field).expect("kind is not a text field");
        self.fields[index].replace_text(value.to_owned());
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
        ProviderFormField::Save => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelListItem {
    id: String,
    owner: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelPickerTarget {
    ActiveProvider,
    ProviderForm,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProviderOverlayAction {
    Consumed,
    Back,
    OpenProviderManager,
    OpenProviderForm,
    OpenProviderEditor(String),
    OpenModelPicker(String),
    BackToProviderForm,
    DiscoverFormModels {
        original_alias: Option<String>,
        values: ProviderFormValues,
    },
    SaveProvider(ProviderFormValues),
    UpdateProvider {
        original_alias: String,
        values: ProviderFormValues,
    },
    RefreshModels(String),
    RefreshFormModels,
    DeleteProvider(String),
    SelectModel {
        alias: String,
        model: String,
        target: ModelPickerTarget,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConfiguredProviderKind, ProviderConfigSource};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn provider_manager_keeps_duplicate_endpoint_profiles_as_distinct_rows() {
        let manager = ProviderManagerOverlay::new(
            vec![
                ProviderListItem::new(
                    "work",
                    "OpenCode Work",
                    ConfiguredProviderKind::OpenAiCompatible,
                    ProviderConfigSource::Managed,
                    Some(OpenAiProtocol::ChatCompletions),
                    Some("model-work"),
                ),
                ProviderListItem::new(
                    "personal",
                    "OpenCode Personal",
                    ConfiguredProviderKind::OpenAiCompatible,
                    ProviderConfigSource::Managed,
                    Some(OpenAiProtocol::ChatCompletions),
                    Some("model-personal"),
                ),
            ],
            Some("work"),
        );

        assert_eq!(manager.items().len(), 2);
        assert_eq!(manager.items()[0].alias(), "work");
        assert_eq!(manager.items()[1].alias(), "personal");
    }

    #[test]
    fn provider_manager_requires_confirmation_before_deleting_managed_provider() {
        let mut manager = ProviderManagerOverlay::new(
            vec![ProviderListItem::new(
                "opencode",
                "OpenCode",
                ConfiguredProviderKind::OpenAiCompatible,
                ProviderConfigSource::Managed,
                Some(OpenAiProtocol::ChatCompletions),
                Some("model-a"),
            )],
            None,
        );
        let delete = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);

        assert_eq!(manager.handle_key(delete), ProviderOverlayAction::Consumed);
        assert!(
            manager
                .notice()
                .is_some_and(|notice| notice.contains("again"))
        );
        assert_eq!(
            manager.handle_key(delete),
            ProviderOverlayAction::DeleteProvider("opencode".to_owned())
        );
    }

    #[test]
    fn provider_manager_separates_edit_and_model_actions() {
        let item = ProviderListItem::new(
            "opencode",
            "OpenCode",
            ConfiguredProviderKind::OpenAiCompatible,
            ProviderConfigSource::Managed,
            Some(OpenAiProtocol::ChatCompletions),
            Some("model-a"),
        );
        let mut edit_manager = ProviderManagerOverlay::new(vec![item.clone()], None);
        let mut model_manager = ProviderManagerOverlay::new(vec![item], None);

        assert_eq!(
            edit_manager.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ProviderOverlayAction::OpenProviderEditor("opencode".to_owned())
        );
        assert_eq!(
            model_manager.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE)),
            ProviderOverlayAction::OpenModelPicker("opencode".to_owned())
        );
    }

    #[test]
    fn provider_manager_refuses_to_delete_user_or_active_provider() {
        let delete = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
        let mut user_manager = ProviderManagerOverlay::new(
            vec![ProviderListItem::new(
                "user-config",
                "User Config",
                ConfiguredProviderKind::OpenAiCompatible,
                ProviderConfigSource::User,
                Some(OpenAiProtocol::ChatCompletions),
                Some("model-a"),
            )],
            None,
        );
        assert_eq!(
            user_manager.handle_key(delete),
            ProviderOverlayAction::Consumed
        );
        assert!(
            user_manager
                .notice()
                .is_some_and(|notice| notice.contains("read-only"))
        );

        let mut active_manager = ProviderManagerOverlay::new(
            vec![ProviderListItem::new(
                "active",
                "Active",
                ConfiguredProviderKind::Anthropic,
                ProviderConfigSource::Managed,
                None,
                Some("model-b"),
            )],
            Some("active"),
        );
        assert_eq!(
            active_manager.handle_key(delete),
            ProviderOverlayAction::Consumed
        );
        assert!(
            active_manager
                .notice()
                .is_some_and(|notice| notice.contains("active"))
        );
    }

    #[test]
    fn provider_form_masks_secret_and_never_debugs_it() {
        let mut form = ProviderFormOverlay::new("opencode".to_owned(), BTreeSet::new());
        form.set_field_for_test(ProviderFormField::ApiKey, "sk-super-secret");

        assert_eq!(form.masked_api_key(), "***************");
        assert!(!format!("{form:?}").contains("sk-super-secret"));
        assert!(format!("{form:?}").contains("<redacted>"));
    }

    #[test]
    fn provider_form_derives_readable_alias_until_the_alias_is_manually_edited() {
        let mut form = ProviderFormOverlay::new("provider".to_owned(), BTreeSet::new());
        for character in "OpenCode Gateway".chars() {
            let _ = form.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }

        assert_eq!(form.field(ProviderFormField::Alias), "opencode-gateway");
    }

    #[test]
    fn provider_form_derives_alias_without_colliding_with_existing_provider() {
        let mut form = ProviderFormOverlay::new(
            "provider".to_owned(),
            BTreeSet::from(["opencode".to_owned()]),
        );
        for character in "OpenCode".chars() {
            let _ = form.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }

        assert_eq!(form.field(ProviderFormField::Alias), "opencode-2");
    }

    #[test]
    fn provider_form_exposes_openai_protocol_selection() {
        let mut form = ProviderFormOverlay::new("provider".to_owned(), BTreeSet::new());
        assert_eq!(form.protocol(), Some(OpenAiProtocol::Responses));
        for _ in 0..3 {
            let _ = form.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }

        let _ = form.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));

        assert_eq!(form.selected_field(), ProviderFormField::Protocol);
        assert_eq!(form.protocol(), Some(OpenAiProtocol::ChatCompletions));
    }

    #[test]
    fn provider_edit_form_prefills_values_retains_secret_and_emits_update() {
        let mut form = ProviderFormOverlay::edit(
            ProviderFormSeed {
                original_alias: "opencode".to_owned(),
                display_name: "OpenCode".to_owned(),
                alias: "opencode".to_owned(),
                kind: ManagedProviderKind::OpenAiCompatible,
                protocol: Some(OpenAiProtocol::Responses),
                base_url: "https://api.openai.com/v1".to_owned(),
                model: "model-a".to_owned(),
            },
            BTreeSet::from(["opencode".to_owned()]),
        );
        assert_eq!(form.field(ProviderFormField::DisplayName), "OpenCode");
        assert_eq!(form.masked_api_key(), "unchanged");

        let _ = form.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        form.insert_paste("-pasted");
        let _ = form.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(form.field(ProviderFormField::Alias), "opencode");
        assert!(
            form.notice()
                .is_some_and(|notice| notice.contains("stable provider ID"))
        );
        let _ = form.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let _ = form.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let _ = form.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        for _ in 0..4 {
            let _ = form.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }

        let action = form.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(
            action,
            ProviderOverlayAction::UpdateProvider { original_alias, values }
                if original_alias == "opencode"
                    && values.protocol == Some(OpenAiProtocol::ChatCompletions)
                    && values.api_key.is_empty()
        ));
    }

    #[test]
    fn provider_form_model_enter_discovers_before_explicit_save() {
        let mut form = ProviderFormOverlay::edit(
            ProviderFormSeed {
                original_alias: "opencode".to_owned(),
                display_name: "OpenCode".to_owned(),
                alias: "opencode".to_owned(),
                kind: ManagedProviderKind::OpenAiCompatible,
                protocol: Some(OpenAiProtocol::ChatCompletions),
                base_url: "https://opencode.example.test/v1".to_owned(),
                model: "model-a".to_owned(),
            },
            BTreeSet::from(["opencode".to_owned()]),
        );
        for _ in 0..6 {
            let _ = form.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }

        let discover = form.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(
            discover,
            ProviderOverlayAction::DiscoverFormModels {
                original_alias: Some(original_alias),
                values,
            } if original_alias == "opencode" && values.model == "model-a"
        ));

        let _ = form.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(form.selected_field(), ProviderFormField::Save);
        assert!(matches!(
            form.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ProviderOverlayAction::UpdateProvider { original_alias, values }
                if original_alias == "opencode" && values.model == "model-a"
        ));
    }

    #[test]
    fn provider_form_ctrl_s_saves_from_any_field() {
        let mut form = ProviderFormOverlay::new("provider".to_owned(), BTreeSet::new());

        let action = form.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));

        assert!(matches!(action, ProviderOverlayAction::SaveProvider(_)));
    }

    #[test]
    fn provider_form_selected_model_replaces_initial_model() {
        let mut form = ProviderFormOverlay::new("provider".to_owned(), BTreeSet::new());

        form.set_model("discovered-model");

        assert_eq!(form.field(ProviderFormField::Model), "discovered-model");
        assert_eq!(form.selected_field(), ProviderFormField::Model);
    }

    #[test]
    fn model_picker_shows_cached_models_while_loading_and_accepts_manual_search() {
        let mut picker = ModelPickerOverlay::new(
            "opencode".to_owned(),
            "OpenCode".to_owned(),
            vec![ModelListItem::new("cached-model", Some("gateway"))],
            true,
        );
        assert!(picker.is_loading());
        assert_eq!(picker.visible_models()[0].id(), "cached-model");

        for character in "manual-model".chars() {
            let _ = picker.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }

        assert_eq!(picker.manual_model(), Some("manual-model"));
    }
}
