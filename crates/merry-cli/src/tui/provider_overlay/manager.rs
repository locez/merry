use super::ProviderOverlayAction;
use crate::config::{ConfiguredProviderKind, ProviderConfigSource};
use crossterm::event::{KeyCode, KeyEvent};
use merry_provider_openai::OpenAiProtocol;

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
                .filter(|item| item.source() == ProviderConfigSource::Managed)
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
