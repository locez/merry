use crate::tui::{
    overlay::{
        MessageDialogKind, MessageDialogOverlay, Overlay, PlanPaletteContext, SettingsOverlay,
        ShortcutsBack,
    },
    provider_overlay::{
        ModelListItem, ModelPickerOverlay, ProviderFormOverlay, ProviderFormSeed,
        ProviderFormValues, ProviderListItem, ProviderManagerOverlay,
    },
    reasoning_picker::ReasoningPickerOverlay,
    state::TuiState,
};
use std::collections::BTreeSet;

/// Owns the visible overlay and its navigation return destinations.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct OverlayState {
    overlay: Option<Overlay>,
    dialog_back: Option<Box<Overlay>>,
    provider_overlay_back: Option<ProviderOverlayBack>,
    provider_form_back: Option<ProviderFormOverlay>,
    reasoning_picker_back: Option<Box<Overlay>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProviderOverlayBack {
    CommandPalette,
    Settings(SettingsOverlay),
}

impl TuiState {
    pub(crate) fn overlay(&self) -> Option<&Overlay> {
        self.overlays.overlay.as_ref()
    }

    pub(crate) fn overlay_mut(&mut self) -> Option<&mut Overlay> {
        self.overlays.overlay.as_mut()
    }

    pub(crate) fn insert_overlay_paste(&mut self, text: &str) -> bool {
        let Some(overlay) = self.overlays.overlay.as_mut() else {
            return false;
        };
        overlay.insert_paste(text);
        true
    }

    pub(crate) fn open_command_palette(&mut self) {
        self.completion_menu = None;
        self.overlays.dialog_back = None;
        self.overlays.provider_overlay_back = None;
        self.overlays.overlay = Some(self.command_palette_overlay());
    }

    pub(crate) fn open_settings(&mut self) {
        self.overlays.dialog_back = None;
        self.overlays.provider_overlay_back = None;
        self.overlays.overlay = Some(Overlay::settings());
    }

    pub(crate) fn open_provider_manager(&mut self, items: Vec<ProviderListItem>) {
        self.overlays.provider_form_back = None;
        self.overlays.reasoning_picker_back = None;
        match self.overlays.overlay.take() {
            Some(Overlay::CommandPalette(_)) => {
                self.overlays.provider_overlay_back = Some(ProviderOverlayBack::CommandPalette);
            }
            Some(Overlay::Settings(settings)) => {
                self.overlays.provider_overlay_back = Some(ProviderOverlayBack::Settings(settings));
            }
            Some(
                Overlay::ProviderManager(_)
                | Overlay::ProviderForm(_)
                | Overlay::ModelPicker(_)
                | Overlay::ReasoningPicker(_),
            ) => {}
            Some(
                Overlay::PlanApproval(_)
                | Overlay::PermissionReview(_)
                | Overlay::Dialog(_)
                | Overlay::Shortcuts(_),
            )
            | None => {
                self.overlays
                    .provider_overlay_back
                    .get_or_insert(ProviderOverlayBack::CommandPalette);
            }
        }
        let current = self.current_provider_alias().map(str::to_owned);
        self.overlays.overlay = Some(Overlay::ProviderManager(ProviderManagerOverlay::new(
            items,
            current.as_deref(),
        )));
    }

    pub(crate) fn open_provider_form(&mut self, alias: String, used_aliases: BTreeSet<String>) {
        self.overlays.provider_form_back = None;
        self.overlays.overlay = Some(Overlay::ProviderForm(ProviderFormOverlay::new(
            alias,
            used_aliases,
        )));
    }

    pub(crate) fn open_provider_editor(
        &mut self,
        seed: ProviderFormSeed,
        used_aliases: BTreeSet<String>,
    ) {
        self.overlays.provider_form_back = None;
        self.overlays.overlay = Some(Overlay::ProviderForm(ProviderFormOverlay::edit(
            seed,
            used_aliases,
        )));
    }

    pub(crate) fn open_model_picker(
        &mut self,
        alias: String,
        display_name: String,
        models: Vec<ModelListItem>,
    ) {
        self.overlays.provider_form_back = None;
        self.overlays.reasoning_picker_back = None;
        self.overlays.overlay = Some(Overlay::ModelPicker(ModelPickerOverlay::new(
            alias,
            display_name,
            models,
            true,
        )));
    }

    pub(crate) fn open_provider_form_model_picker(
        &mut self,
        alias: String,
        display_name: String,
    ) -> bool {
        let form = match self.overlays.overlay.take() {
            Some(Overlay::ProviderForm(form)) => form,
            overlay => {
                self.overlays.overlay = overlay;
                return false;
            }
        };
        self.overlays.provider_form_back = Some(form);
        self.overlays.overlay = Some(Overlay::ModelPicker(ModelPickerOverlay::for_provider_form(
            alias,
            display_name,
            Vec::new(),
        )));
        true
    }

    pub(crate) fn open_reasoning_picker(
        &mut self,
        alias: String,
        model: String,
        target: crate::tui::provider_overlay::ModelPickerTarget,
    ) -> bool {
        let Some(previous) = self.overlays.overlay.take() else {
            return false;
        };
        self.overlays.reasoning_picker_back = Some(Box::new(previous));
        self.overlays.overlay = Some(Overlay::ReasoningPicker(ReasoningPickerOverlay::new(
            alias, model, target,
        )));
        true
    }

    pub(crate) fn provider_form_discovery_request(
        &self,
    ) -> Option<(Option<String>, ProviderFormValues)> {
        self.overlays
            .provider_form_back
            .as_ref()
            .map(ProviderFormOverlay::discovery_request)
    }

    pub(crate) fn select_provider_form_model_with_reasoning(
        &mut self,
        model: &str,
        reasoning_effort: &str,
    ) -> bool {
        let Some(mut form) = self.overlays.provider_form_back.take() else {
            return false;
        };
        form.set_model_and_reasoning(model, reasoning_effort);
        self.overlays.overlay = Some(Overlay::ProviderForm(form));
        self.overlays.reasoning_picker_back = None;
        true
    }

    pub(crate) fn restore_settings_after_reasoning_picker(&mut self) -> bool {
        let Some(back) = self.overlays.reasoning_picker_back.take() else {
            return false;
        };
        match *back {
            Overlay::Settings(settings) => {
                self.overlays.overlay = Some(Overlay::Settings(settings));
                true
            }
            overlay => {
                self.overlays.reasoning_picker_back = Some(Box::new(overlay));
                false
            }
        }
    }

    pub(crate) fn update_model_picker(
        &mut self,
        alias: &str,
        result: Result<Vec<ModelListItem>, String>,
    ) {
        match result {
            Ok(models) => {
                if let Some(Overlay::ModelPicker(picker)) = self.overlays.overlay.as_mut()
                    && picker.alias() == alias
                {
                    picker.set_models(models);
                }
            }
            Err(error) => {
                if self.overlays.overlay.as_ref().is_some_and(
                    |overlay| matches!(overlay, Overlay::ModelPicker(picker) if picker.alias() == alias),
                ) {
                    self.show_error_dialog("Model discovery failed", error);
                }
            }
        }
    }

    pub(crate) fn mark_model_picker_loading(&mut self, alias: &str) {
        if let Some(Overlay::ModelPicker(picker)) = self.overlays.overlay.as_mut()
            && picker.alias() == alias
        {
            picker.set_loading();
        }
    }

    pub(crate) fn set_provider_overlay_error(&mut self, error: String) {
        self.show_error_dialog("Provider error", error);
    }

    pub(crate) fn show_info_dialog(&mut self, title: &str, message: String) {
        self.show_dialog(MessageDialogKind::Info, title, message);
    }

    pub(crate) fn open_plan_approval(&mut self) {
        match (self.plan.approval_summary(), self.plan.approval_input()) {
            (Ok(message), Ok(input)) => {
                if !matches!(self.overlays.overlay, Some(Overlay::PlanApproval(_))) {
                    self.overlays.dialog_back = self.overlays.overlay.take().map(Box::new);
                }
                self.overlays.overlay = Some(Overlay::plan_approval(message, input));
            }
            (Err(error), _) | (_, Err(error)) => {
                self.show_error_dialog("Plan approval unavailable", error)
            }
        }
    }

    pub(crate) fn open_permission_review(&mut self, approval_id: String, body: String) {
        self.completion_menu = None;
        self.overlays.dialog_back = None;
        self.overlays.provider_overlay_back = None;
        self.overlays.overlay = Some(Overlay::permission_review(approval_id, body));
    }

    pub(crate) fn plan_approval_input(&self) -> Option<merry_runtime::PlanApprovalInput> {
        match self.overlays.overlay.as_ref() {
            Some(Overlay::PlanApproval(approval)) => Some(approval.input().clone()),
            _ => None,
        }
    }

    pub(crate) fn show_error_dialog(&mut self, title: &str, message: String) {
        self.show_dialog(MessageDialogKind::Error, title, message);
    }

    pub(super) fn show_dialog(&mut self, kind: MessageDialogKind, title: &str, message: String) {
        if !matches!(self.overlays.overlay, Some(Overlay::Dialog(_))) {
            self.overlays.dialog_back = self.overlays.overlay.take().map(Box::new);
        }
        self.overlays.overlay = Some(Overlay::Dialog(MessageDialogOverlay::new(
            kind, title, message,
        )));
    }

    pub(crate) fn open_shortcuts(&mut self) {
        let back = match self.overlays.overlay.take() {
            Some(Overlay::Settings(settings)) => ShortcutsBack::Settings(settings),
            _ => ShortcutsBack::CommandPalette,
        };
        self.overlays.overlay = Some(Overlay::Shortcuts(back));
    }

    pub(crate) fn close_overlay(&mut self) {
        self.overlays.overlay = None;
        self.overlays.dialog_back = None;
        self.overlays.provider_overlay_back = None;
        self.overlays.provider_form_back = None;
        self.overlays.reasoning_picker_back = None;
    }

    pub(crate) fn back_overlay(&mut self) {
        let command_palette = self.command_palette_overlay();
        self.overlays.overlay = match self.overlays.overlay.take() {
            Some(Overlay::Shortcuts(ShortcutsBack::Settings(settings))) => {
                Some(Overlay::Settings(settings))
            }
            Some(Overlay::Shortcuts(ShortcutsBack::CommandPalette))
            | Some(Overlay::Settings(_)) => Some(command_palette.clone()),
            Some(Overlay::ProviderManager(_)) => match self.overlays.provider_overlay_back.take() {
                Some(ProviderOverlayBack::Settings(settings)) => Some(Overlay::Settings(settings)),
                Some(ProviderOverlayBack::CommandPalette) | None => Some(command_palette),
            },
            Some(Overlay::PlanApproval(_) | Overlay::PermissionReview(_) | Overlay::Dialog(_)) => {
                self.overlays.dialog_back.take().map(|overlay| *overlay)
            }
            Some(Overlay::ModelPicker(_)) => self
                .overlays
                .provider_form_back
                .take()
                .map(Overlay::ProviderForm),
            Some(Overlay::ReasoningPicker(_)) => self
                .overlays
                .reasoning_picker_back
                .take()
                .map(|overlay| *overlay),
            _ => None,
        };
    }

    pub(super) fn command_palette_overlay(&self) -> Overlay {
        Overlay::command_palette_for_plan(PlanPaletteContext::from_snapshot(
            self.plan.snapshot(),
            self.plan.selected_node_id(),
            self.plan.is_open(),
            self.plan.is_focused(),
        ))
    }
}
