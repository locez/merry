use super::{ProviderFormField, ProviderFormOverlay, ProviderFormSeed, ProviderOverlayAction};
use crate::config::{ConfiguredProviderKind, ManagedProviderKind, derive_provider_alias};
use crossterm::event::{KeyCode, KeyEvent};
use merry_llm::{ReasoningEffort, ServiceTier};
use merry_provider_openai::OpenAiProtocol;
use std::collections::BTreeSet;

/// Which pane currently owns keyboard input in the two-column Provider Workbench.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ProviderWorkbenchFocus {
    #[default]
    List,
    Detail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderListItem {
    alias: String,
    display_name: String,
    kind: ConfiguredProviderKind,
    protocol: Option<OpenAiProtocol>,
    model: Option<String>,
    base_url: Option<String>,
    reasoning_effort: Option<String>,
    service_tier: Option<ServiceTier>,
}

impl ProviderListItem {
    pub(crate) fn new(
        alias: &str,
        display_name: &str,
        kind: ConfiguredProviderKind,
        protocol: Option<OpenAiProtocol>,
        model: Option<&str>,
    ) -> Self {
        Self {
            alias: alias.to_owned(),
            display_name: display_name.to_owned(),
            kind,
            protocol,
            model: model.map(str::to_owned),
            base_url: None,
            reasoning_effort: None,
            service_tier: None,
        }
    }

    pub(crate) fn with_details(
        mut self,
        base_url: Option<String>,
        reasoning_effort: Option<String>,
    ) -> Self {
        self.base_url = base_url;
        self.reasoning_effort = reasoning_effort;
        self
    }

    pub(crate) fn with_service_tier(mut self, service_tier: Option<ServiceTier>) -> Self {
        self.service_tier = service_tier;
        self
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

    pub(crate) fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub(crate) fn protocol(&self) -> Option<OpenAiProtocol> {
        self.protocol
    }

    fn to_form_seed(&self) -> ProviderFormSeed {
        let kind = match self.kind {
            ConfiguredProviderKind::OpenAiCompatible => ManagedProviderKind::OpenAiCompatible,
            ConfiguredProviderKind::Anthropic => ManagedProviderKind::Anthropic,
        };
        let default_url = match kind {
            ManagedProviderKind::OpenAiCompatible => "https://api.openai.com/v1",
            ManagedProviderKind::Anthropic => "https://api.anthropic.com",
        };
        let reasoning_effort = self
            .reasoning_effort
            .as_deref()
            .and_then(|value| ReasoningEffort::new(value).ok());
        ProviderFormSeed {
            original_alias: self.alias.clone(),
            display_name: self.display_name.clone(),
            alias: self.alias.clone(),
            kind,
            protocol: self.protocol,
            base_url: self
                .base_url
                .clone()
                .unwrap_or_else(|| default_url.to_owned()),
            model: self.model.clone().unwrap_or_default(),
            reasoning_effort,
            service_tier: self.service_tier,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderManagerOverlay {
    items: Vec<ProviderListItem>,
    selected: usize,
    current_alias: Option<String>,
    notice: Option<String>,
    pending_delete_alias: Option<String>,
    focus: ProviderWorkbenchFocus,
    adding_new: bool,
    detail_form: Option<ProviderFormOverlay>,
}

impl ProviderManagerOverlay {
    pub(crate) fn new(items: Vec<ProviderListItem>, current_alias: Option<&str>) -> Self {
        let selected = current_alias
            .and_then(|alias| items.iter().position(|item| item.alias() == alias))
            .unwrap_or(0);
        let mut manager = Self {
            items,
            selected,
            current_alias: current_alias.map(str::to_owned),
            notice: None,
            pending_delete_alias: None,
            focus: ProviderWorkbenchFocus::List,
            adding_new: false,
            detail_form: None,
        };
        manager.sync_detail_form();
        manager
    }

    pub(crate) fn items(&self) -> &[ProviderListItem] {
        &self.items
    }

    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    pub(crate) fn selected_item(&self) -> Option<&ProviderListItem> {
        self.items.get(self.selected)
    }

    pub(crate) fn focus(&self) -> ProviderWorkbenchFocus {
        self.focus
    }

    pub(crate) fn is_adding(&self) -> bool {
        self.adding_new
    }

    pub(crate) fn detail_form(&self) -> Option<&ProviderFormOverlay> {
        self.detail_form.as_ref()
    }

    pub(crate) fn detail_form_mut(&mut self) -> Option<&mut ProviderFormOverlay> {
        self.detail_form.as_mut()
    }

    pub(crate) fn select(&mut self, index: usize) {
        if self.items.is_empty() {
            return;
        }
        self.selected = index.min(self.items.len() - 1);
        self.adding_new = false;
        self.focus = ProviderWorkbenchFocus::List;
        self.pending_delete_alias = None;
        self.notice = None;
        self.sync_detail_form();
    }

    pub(crate) fn start_inline_add(&mut self) {
        let used = self.used_aliases();
        let alias = derive_provider_alias("Provider", &used)
            .map(|alias| alias.as_str().to_owned())
            .unwrap_or_else(|_| "provider".to_owned());
        self.adding_new = true;
        self.pending_delete_alias = None;
        self.notice = None;
        self.detail_form = Some(ProviderFormOverlay::new(alias, used));
        self.focus = ProviderWorkbenchFocus::Detail;
    }

    pub(crate) fn focus_detail(&mut self) -> bool {
        if self.detail_form.is_some() {
            self.focus = ProviderWorkbenchFocus::Detail;
            self.notice = None;
            true
        } else {
            false
        }
    }

    pub(crate) fn focus_list(&mut self) {
        let was_adding = self.adding_new;
        self.adding_new = false;
        self.focus = ProviderWorkbenchFocus::List;
        self.notice = None;
        if was_adding {
            self.sync_detail_form();
        }
    }

    pub(crate) fn click_detail_field(&mut self, field: ProviderFormField) -> ProviderOverlayAction {
        let Some(form) = self.detail_form.as_mut() else {
            return ProviderOverlayAction::Consumed;
        };
        self.focus = ProviderWorkbenchFocus::Detail;
        self.notice = None;
        form.click_field(field)
    }

    pub(crate) fn trigger_detail_model_discovery(&mut self) -> ProviderOverlayAction {
        let Some(form) = self.detail_form.as_mut() else {
            return ProviderOverlayAction::Consumed;
        };
        self.focus = ProviderWorkbenchFocus::Detail;
        form.select_field(ProviderFormField::Model);
        form.trigger_model_discovery()
    }

    pub(crate) fn trigger_detail_copy_api_key(&mut self) -> ProviderOverlayAction {
        let Some(form) = self.detail_form.as_mut() else {
            return ProviderOverlayAction::Consumed;
        };
        self.focus = ProviderWorkbenchFocus::Detail;
        form.trigger_copy_api_key()
    }

    pub(crate) fn insert_paste(&mut self, text: &str) {
        if self.focus == ProviderWorkbenchFocus::Detail
            && let Some(form) = self.detail_form.as_mut()
        {
            form.insert_paste(text);
        }
    }

    pub(crate) fn trigger_delete_selected(&mut self) -> ProviderOverlayAction {
        let Some(item) = self.items.get(self.selected) else {
            return ProviderOverlayAction::Consumed;
        };
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

    pub(crate) fn current_alias(&self) -> Option<&str> {
        self.current_alias.as_deref()
    }

    pub(crate) fn notice(&self) -> Option<&str> {
        if self.focus == ProviderWorkbenchFocus::Detail
            && let Some(form_notice) = self
                .detail_form
                .as_ref()
                .and_then(ProviderFormOverlay::notice)
        {
            return Some(form_notice);
        }
        self.notice.as_deref()
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> ProviderOverlayAction {
        if self.focus == ProviderWorkbenchFocus::Detail
            && let Some(form) = self.detail_form.as_mut()
        {
            if key.code == KeyCode::Esc {
                self.focus_list();
                return ProviderOverlayAction::Consumed;
            }
            return form.handle_key(key);
        }

        match key.code {
            KeyCode::Esc => ProviderOverlayAction::Back,
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                self.adding_new = false;
                self.pending_delete_alias = None;
                self.notice = None;
                self.sync_detail_form();
                ProviderOverlayAction::Consumed
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.items.len().saturating_sub(1));
                self.adding_new = false;
                self.pending_delete_alias = None;
                self.notice = None;
                self.sync_detail_form();
                ProviderOverlayAction::Consumed
            }
            KeyCode::Tab | KeyCode::Right => {
                let _ = self.focus_detail();
                ProviderOverlayAction::Consumed
            }
            KeyCode::Enter => self.items.get(self.selected).map_or(
                ProviderOverlayAction::OpenProviderForm,
                |item| match item.model() {
                    Some(_) => ProviderOverlayAction::SelectProvider(item.alias().to_owned()),
                    None => ProviderOverlayAction::OpenModelPicker(item.alias().to_owned()),
                },
            ),
            KeyCode::Char('e') => self
                .items
                .get(self.selected)
                .map_or(ProviderOverlayAction::Consumed, |item| {
                    ProviderOverlayAction::OpenProviderEditor(item.alias().to_owned())
                }),
            KeyCode::Char('m') => self
                .items
                .get(self.selected)
                .map_or(ProviderOverlayAction::Consumed, |item| {
                    ProviderOverlayAction::OpenModelPicker(item.alias.clone())
                }),
            KeyCode::Char('n') => ProviderOverlayAction::OpenProviderForm,
            KeyCode::Char('d') => self.trigger_delete_selected(),
            _ => ProviderOverlayAction::Consumed,
        }
    }

    fn used_aliases(&self) -> BTreeSet<String> {
        self.items
            .iter()
            .map(|item| item.alias().to_owned())
            .collect()
    }

    fn sync_detail_form(&mut self) {
        let used = self.used_aliases();
        self.detail_form = self
            .items
            .get(self.selected)
            .map(|item| ProviderFormOverlay::edit(item.to_form_seed(), used));
    }
}
