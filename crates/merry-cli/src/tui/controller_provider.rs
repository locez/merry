//! Provider overlay effects and model discovery for the TUI controller.

use super::{
    controller::{ControllerEffect, ModelDiscoveryCompletion, ProviderController},
    preferences::{TuiPreferencesStore, TuiSettingsDefaults},
    provider_overlay::{
        ModelListItem, ModelPickerTarget, ProviderDraftMode, ProviderFormSeed, ProviderFormValues,
        ProviderListItem, ProviderOverlayAction,
    },
    runtime::TuiRuntimeSession,
    state::TuiState,
};
use crate::{
    cli_error::{CliError, unexpected},
    config::{ProviderAlias, derive_provider_alias},
    provider_management::{
        ProviderDiscoveryDraft, ProviderManagementError, ProviderManagementService,
    },
};
use std::collections::BTreeSet;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(super) fn provider_overlay_effect(action: ProviderOverlayAction) -> ControllerEffect {
    match action {
        ProviderOverlayAction::Consumed | ProviderOverlayAction::Back => ControllerEffect::None,
        ProviderOverlayAction::OpenProviderManager => ControllerEffect::OpenProviderManager,
        ProviderOverlayAction::OpenProviderForm => ControllerEffect::OpenProviderForm,
        ProviderOverlayAction::OpenProviderEditor(alias) => {
            ControllerEffect::OpenProviderEditor(alias)
        }
        ProviderOverlayAction::OpenModelPicker(alias) => ControllerEffect::OpenModelPicker(alias),
        ProviderOverlayAction::BackToProviderForm => ControllerEffect::BackToProviderForm,
        ProviderOverlayAction::DiscoverFormModels {
            original_alias,
            values,
        } => ControllerEffect::DiscoverFormModels {
            original_alias,
            values,
        },
        ProviderOverlayAction::SaveProvider(values) => ControllerEffect::SaveProvider(values),
        ProviderOverlayAction::UpdateProvider {
            original_alias,
            values,
        } => ControllerEffect::UpdateProvider {
            original_alias,
            values,
        },
        ProviderOverlayAction::RefreshModels(alias) => ControllerEffect::RefreshModels(alias),
        ProviderOverlayAction::RefreshFormModels => ControllerEffect::RefreshFormModels,
        ProviderOverlayAction::DeleteProvider(alias) => ControllerEffect::DeleteProvider(alias),
        ProviderOverlayAction::SelectProvider(alias) => ControllerEffect::SelectProvider { alias },
        ProviderOverlayAction::SelectModel {
            alias,
            model,
            target,
        } => ControllerEffect::OpenReasoningPicker {
            alias,
            model,
            target,
        },
        ProviderOverlayAction::SelectReasoning {
            alias,
            model,
            reasoning_effort,
            target,
        } => ControllerEffect::ApplyProviderModel {
            alias,
            model,
            reasoning_effort,
            target,
        },
    }
}

pub(super) fn is_provider_effect(effect: &ControllerEffect) -> bool {
    matches!(
        effect,
        ControllerEffect::OpenProviderManager
            | ControllerEffect::OpenProviderForm
            | ControllerEffect::OpenProviderEditor(_)
            | ControllerEffect::OpenModelPicker(_)
            | ControllerEffect::BackToProviderForm
            | ControllerEffect::DiscoverFormModels { .. }
            | ControllerEffect::RefreshModels(_)
            | ControllerEffect::RefreshFormModels
            | ControllerEffect::DeleteProvider(_)
            | ControllerEffect::SaveProvider(_)
            | ControllerEffect::UpdateProvider { .. }
            | ControllerEffect::SelectProvider { .. }
            | ControllerEffect::OpenReasoningPicker { .. }
            | ControllerEffect::ApplyProviderModel { .. }
    )
}

pub(super) async fn dispatch_provider_effect(
    effect: ControllerEffect,
    session: &mut TuiRuntimeSession,
    state: &mut TuiState,
    preferences_store: &TuiPreferencesStore,
    providers: ProviderController<'_>,
) -> Result<bool, CliError> {
    match effect {
        ControllerEffect::OpenProviderManager => {
            cancel_model_discovery(providers.discovery_token);
            let items = provider_list_items(providers.management, state)?;
            state.open_provider_manager(items);
            Ok(false)
        }
        ControllerEffect::OpenProviderForm => {
            cancel_model_discovery(providers.discovery_token);
            let used = providers
                .management
                .profiles()
                .map_err(unexpected)?
                .into_iter()
                .map(|profile| profile.alias().as_str().to_owned())
                .collect::<BTreeSet<_>>();
            let alias = derive_provider_alias("Provider", &used)
                .map_err(unexpected)?
                .as_str()
                .to_owned();
            state.open_provider_form(alias, used);
            Ok(false)
        }
        ControllerEffect::OpenProviderEditor(alias) => {
            cancel_model_discovery(providers.discovery_token);
            let alias = ProviderAlias::new(&alias).map_err(unexpected)?;
            let editable = match providers.management.editable_provider(&alias) {
                Ok(editable) => editable,
                Err(error) => {
                    if matches!(&error, ProviderManagementError::ReadOnlyProvider { .. }) {
                        state.show_info_dialog("Read-only provider", error.to_string());
                    } else {
                        state.set_provider_overlay_error(error.to_string());
                    }
                    return Ok(false);
                }
            };
            let used = providers
                .management
                .profiles()
                .map_err(unexpected)?
                .into_iter()
                .map(|profile| profile.alias().as_str().to_owned())
                .collect::<BTreeSet<_>>();
            state.open_provider_editor(
                ProviderFormSeed {
                    original_alias: editable.alias.as_str().to_owned(),
                    display_name: editable.display_name,
                    alias: editable.alias.as_str().to_owned(),
                    kind: editable.kind,
                    protocol: editable.protocol,
                    base_url: editable.base_url,
                    model: editable.default_model.as_str().to_owned(),
                    reasoning_effort: editable.reasoning_effort,
                },
                used,
            );
            Ok(false)
        }
        ControllerEffect::OpenModelPicker(alias) => {
            open_model_picker(
                &alias,
                providers.management,
                state,
                providers.discovery_tx,
                providers.discovery_generation,
                providers.discovery_token,
            )
            .await?;
            Ok(false)
        }
        ControllerEffect::BackToProviderForm => {
            cancel_model_discovery(providers.discovery_token);
            state.back_overlay();
            Ok(false)
        }
        ControllerEffect::DiscoverFormModels {
            original_alias,
            values,
        } => {
            cancel_model_discovery(providers.discovery_token);
            let draft = match provider_discovery_draft(original_alias.as_deref(), &values) {
                Ok(draft) => draft,
                Err(error) => {
                    state.set_provider_overlay_error(error.to_string());
                    return Ok(false);
                }
            };
            let alias = draft.alias().as_str().to_owned();
            if !state.open_provider_form_model_picker(alias.clone(), values.display_name.clone()) {
                state.set_provider_overlay_error(
                    "provider form is no longer available for model discovery".to_owned(),
                );
                return Ok(false);
            }
            start_form_model_discovery(
                alias,
                draft,
                providers.management.clone(),
                providers.discovery_tx.clone(),
                providers.discovery_generation,
                providers.discovery_token,
            );
            Ok(false)
        }
        ControllerEffect::RefreshModels(alias) => {
            state.mark_model_picker_loading(&alias);
            start_model_discovery(
                ProviderAlias::new(&alias).map_err(unexpected)?,
                providers.management.clone(),
                providers.discovery_tx.clone(),
                providers.discovery_generation,
                providers.discovery_token,
            );
            Ok(false)
        }
        ControllerEffect::RefreshFormModels => {
            let Some((original_alias, values)) = state.provider_form_discovery_request() else {
                state.set_provider_overlay_error(
                    "provider form is no longer available for model discovery".to_owned(),
                );
                return Ok(false);
            };
            let draft = match provider_discovery_draft(original_alias.as_deref(), &values) {
                Ok(draft) => draft,
                Err(error) => {
                    state.set_provider_overlay_error(error.to_string());
                    return Ok(false);
                }
            };
            let alias = draft.alias().as_str().to_owned();
            state.mark_model_picker_loading(&alias);
            start_form_model_discovery(
                alias,
                draft,
                providers.management.clone(),
                providers.discovery_tx.clone(),
                providers.discovery_generation,
                providers.discovery_token,
            );
            Ok(false)
        }
        ControllerEffect::DeleteProvider(alias) => {
            let alias = ProviderAlias::new(&alias).map_err(unexpected)?;
            if state.current_provider_alias() == Some(alias.as_str()) {
                state.set_provider_overlay_error(
                    "switch provider before deleting the active one".to_owned(),
                );
                return Ok(false);
            }
            if let Err(error) = providers.management.delete_provider(&alias).await {
                state.set_provider_overlay_error(error.to_string());
                return Ok(false);
            }
            let mut preferences = state.preferences().clone();
            preferences
                .clear_provider_state(alias.as_str())
                .map_err(unexpected)?;
            let preference_cleanup_error = preferences_store.save(&preferences).await.err();
            state.replace_preferences(preferences);
            if let Some(config) = providers.management.config().cloned() {
                session.replace_config(config.clone());
                state.replace_settings_defaults(
                    TuiSettingsDefaults::from_config(Some(&config)).map_err(unexpected)?,
                );
            }
            let items = provider_list_items(providers.management, state)?;
            state.open_provider_manager(items);
            if let Some(error) = preference_cleanup_error {
                state.set_provider_overlay_error(format!(
                    "Provider deleted, but its saved model preference could not be removed: {error}"
                ));
            }
            Ok(false)
        }
        ControllerEffect::SaveProvider(values) => {
            let (alias, model, draft) = match values.to_draft(ProviderDraftMode::Create) {
                Ok(values) => values,
                Err(error) => {
                    state.set_provider_overlay_error(error.to_string());
                    return Ok(false);
                }
            };
            let reasoning_effort = draft.reasoning_effort().cloned();
            if let Err(error) = providers.management.save_provider(draft).await {
                state.set_provider_overlay_error(error.to_string());
                return Ok(false);
            }
            let config = providers
                .management
                .config()
                .cloned()
                .ok_or_else(|| unexpected("saved provider config did not reload"))?;
            session.replace_config(config.clone());
            state.replace_settings_defaults(
                TuiSettingsDefaults::from_config(Some(&config)).map_err(unexpected)?,
            );
            let mut preferences = state.preferences().clone();
            preferences.provider = Some(alias.as_str().to_owned());
            preferences
                .set_model_for_provider(alias.as_str(), Some(model.as_str()))
                .map_err(unexpected)?;
            preferences
                .set_reasoning_effort_for_provider(alias.as_str(), reasoning_effort)
                .map_err(unexpected)?;
            if let Err(error) = session.apply_preferences(&preferences).await {
                state.set_provider_overlay_error(format!(
                    "provider saved; switch failed: {error:?}"
                ));
                return Ok(false);
            }
            preferences_store
                .save(&preferences)
                .await
                .map_err(unexpected)?;
            state.replace_preferences(preferences);
            state.set_model_label(session.model_label.clone());
            state.set_reasoning_effort_label(session.reasoning_effort_label.clone());
            let items = provider_list_items(providers.management, state)?;
            state.open_provider_manager(items);
            Ok(false)
        }
        ControllerEffect::UpdateProvider {
            original_alias,
            values,
        } => {
            let original_alias = ProviderAlias::new(&original_alias).map_err(unexpected)?;
            let (_, _, draft) = match values.to_draft(ProviderDraftMode::Update) {
                Ok(values) => values,
                Err(error) => {
                    state.set_provider_overlay_error(error.to_string());
                    return Ok(false);
                }
            };
            if let Err(error) = providers
                .management
                .update_provider(&original_alias, draft)
                .await
            {
                state.set_provider_overlay_error(error.to_string());
                return Ok(false);
            }
            let config = providers
                .management
                .config()
                .cloned()
                .ok_or_else(|| unexpected("updated provider config did not reload"))?;
            session.replace_config(config.clone());
            state.replace_settings_defaults(
                TuiSettingsDefaults::from_config(Some(&config)).map_err(unexpected)?,
            );
            // Provider configuration and the TUI's per-provider selection are
            // separate stores. Editing a URL, key, or provider default must
            // not replace the model/thinking pair selected for this provider.
            // Model changes go through the model picker, which atomically
            // updates the selection preferences.
            let preferences = state.preferences().clone();
            if let Err(error) = session.apply_preferences(&preferences).await {
                state.set_provider_overlay_error(format!(
                    "provider updated; runtime refresh failed: {error:?}"
                ));
                return Ok(false);
            }
            state.set_model_label(session.model_label.clone());
            state.set_reasoning_effort_label(session.reasoning_effort_label.clone());
            let items = provider_list_items(providers.management, state)?;
            state.open_provider_manager(items);
            Ok(false)
        }
        ControllerEffect::SelectProvider { alias } => {
            let alias = ProviderAlias::new(&alias).map_err(unexpected)?;
            let mut preferences = state.preferences().clone();
            preferences.provider = Some(alias.as_str().to_owned());
            if let Err(error) = session.apply_preferences(&preferences).await {
                state.set_provider_overlay_error(format!("switch failed: {error:?}"));
                return Ok(false);
            }
            preferences_store
                .save(&preferences)
                .await
                .map_err(unexpected)?;
            state.replace_preferences(preferences);
            state.set_model_label(session.model_label.clone());
            state.set_reasoning_effort_label(session.reasoning_effort_label.clone());
            cancel_model_discovery(providers.discovery_token);
            let items = provider_list_items(providers.management, state)?;
            state.open_provider_manager(items);
            Ok(false)
        }
        ControllerEffect::OpenReasoningPicker {
            alias,
            model,
            target,
        } => {
            if !state.open_reasoning_picker(alias, model, target) {
                state.set_provider_overlay_error(
                    "model selection is no longer available for reasoning mode selection"
                        .to_owned(),
                );
            }
            Ok(false)
        }
        ControllerEffect::ApplyProviderModel {
            alias,
            model,
            reasoning_effort,
            target,
        } => match target {
            ModelPickerTarget::ActiveProvider => {
                let alias = ProviderAlias::new(&alias).map_err(unexpected)?;
                let mut preferences = state.preferences().clone();
                preferences.provider = Some(alias.as_str().to_owned());
                preferences
                    .set_model_and_reasoning_for_provider(alias.as_str(), &model, reasoning_effort)
                    .map_err(unexpected)?;
                if let Err(error) = session.apply_preferences(&preferences).await {
                    state.set_provider_overlay_error(format!("switch failed: {error:?}"));
                    return Ok(false);
                }
                preferences_store
                    .save(&preferences)
                    .await
                    .map_err(unexpected)?;
                state.replace_preferences(preferences);
                state.set_model_label(session.model_label.clone());
                state.set_reasoning_effort_label(session.reasoning_effort_label.clone());
                if !state.restore_settings_after_reasoning_picker() {
                    cancel_model_discovery(providers.discovery_token);
                    let items = provider_list_items(providers.management, state)?;
                    state.open_provider_manager(items);
                }
                Ok(false)
            }
            ModelPickerTarget::ProviderForm => {
                cancel_model_discovery(providers.discovery_token);
                if !state
                    .select_provider_form_model_with_reasoning(&model, reasoning_effort.as_str())
                {
                    state.set_provider_overlay_error(
                        "provider form is no longer available for the selected model".to_owned(),
                    );
                }
                Ok(false)
            }
        },
        _ => unreachable!("non-provider effect passed to provider dispatcher"),
    }
}

fn provider_list_items(
    provider_management: &ProviderManagementService,
    state: &TuiState,
) -> Result<Vec<ProviderListItem>, CliError> {
    provider_management
        .profiles()
        .map_err(unexpected)?
        .into_iter()
        .map(|profile| {
            let model = state
                .preferences()
                .model_for_provider(profile.alias().as_str())
                .or_else(|| profile.default_model().map(merry_llm::ModelName::as_str));
            Ok(ProviderListItem::new(
                profile.alias().as_str(),
                profile.display_name(),
                profile.kind(),
                profile.source(),
                profile.protocol(),
                model,
            ))
        })
        .collect()
}

async fn open_model_picker(
    alias: &str,
    provider_management: &ProviderManagementService,
    state: &mut TuiState,
    model_discovery_tx: &mpsc::Sender<ModelDiscoveryCompletion>,
    model_discovery_generation: &mut u64,
    model_discovery_token: &mut Option<CancellationToken>,
) -> Result<(), CliError> {
    let alias = ProviderAlias::new(alias).map_err(unexpected)?;
    let profile = provider_management
        .config()
        .ok_or_else(|| unexpected("no provider config is loaded"))?
        .provider_profile(alias.as_str())
        .map_err(unexpected)?;
    let cached = provider_management
        .load_model_cache(&alias)
        .await
        .map_err(unexpected)?
        .map(model_list_items)
        .unwrap_or_default();
    state.open_model_picker(
        alias.as_str().to_owned(),
        profile.display_name().to_owned(),
        cached,
    );
    start_model_discovery(
        alias,
        provider_management.clone(),
        model_discovery_tx.clone(),
        model_discovery_generation,
        model_discovery_token,
    );
    Ok(())
}

fn start_model_discovery(
    alias: ProviderAlias,
    provider_management: ProviderManagementService,
    model_discovery_tx: mpsc::Sender<ModelDiscoveryCompletion>,
    model_discovery_generation: &mut u64,
    model_discovery_token: &mut Option<CancellationToken>,
) {
    cancel_model_discovery(model_discovery_token);
    *model_discovery_generation = model_discovery_generation.wrapping_add(1);
    let generation = *model_discovery_generation;
    let token = CancellationToken::new();
    *model_discovery_token = Some(token.clone());
    tokio::spawn(async move {
        let alias_text = alias.as_str().to_owned();
        let result = provider_management
            .discover_and_cache(&alias, token)
            .await
            .map(model_list_items)
            .map_err(|error| error.to_string());
        let _ = model_discovery_tx
            .send(ModelDiscoveryCompletion {
                generation,
                alias: alias_text,
                result,
            })
            .await;
    });
}

pub(super) fn provider_discovery_draft(
    original_alias: Option<&str>,
    values: &ProviderFormValues,
) -> Result<ProviderDiscoveryDraft, ProviderManagementError> {
    let alias = ProviderAlias::new(values.alias.trim())?;
    let original_alias = original_alias.map(ProviderAlias::new).transpose()?;
    let api_key = (!values.api_key.trim().is_empty()).then(|| values.api_key.trim());
    ProviderDiscoveryDraft::new(
        alias,
        original_alias,
        values.kind,
        values.protocol,
        values.base_url.trim(),
        api_key,
    )
}

fn start_form_model_discovery(
    alias: String,
    draft: ProviderDiscoveryDraft,
    provider_management: ProviderManagementService,
    model_discovery_tx: mpsc::Sender<ModelDiscoveryCompletion>,
    model_discovery_generation: &mut u64,
    model_discovery_token: &mut Option<CancellationToken>,
) {
    cancel_model_discovery(model_discovery_token);
    *model_discovery_generation = model_discovery_generation.wrapping_add(1);
    let generation = *model_discovery_generation;
    let token = CancellationToken::new();
    *model_discovery_token = Some(token.clone());
    tokio::spawn(async move {
        let result = provider_management
            .discover_from_draft(draft, token)
            .await
            .map(model_list_items)
            .map_err(|error| error.to_string());
        let _ = model_discovery_tx
            .send(ModelDiscoveryCompletion {
                generation,
                alias,
                result,
            })
            .await;
    });
}

fn cancel_model_discovery(token: &mut Option<CancellationToken>) {
    if let Some(token) = token.take() {
        token.cancel();
    }
}

fn model_list_items(catalog: merry_llm::ModelCatalog) -> Vec<ModelListItem> {
    catalog
        .into_models()
        .into_iter()
        .map(|model| ModelListItem::new(model.id().as_str(), model.owner()))
        .collect()
}
