use crate::cli_error::CliError;
use crate::coding_runtime::ProcessExecutionMode;
use crate::config::{MerryConfig, XdgPaths};
use crate::provider_management::{
    ProviderDiscoveryDraft, ProviderManagementError, ProviderManagementService,
};
use crate::sandbox::ChildHandoff as SandboxChildHandoff;
use crate::{
    config::{ProviderAlias, derive_provider_alias},
    provider_management::ProviderDraft,
};
use crossterm::event::KeyCode;
use input_history_store::InputHistoryStore;
use keymap::Keymap;
use preferences::{TuiPreferences, TuiPreferencesStore, TuiSettingsDefaults};
use projector::TuiProjector;
use provider_overlay::{
    ModelListItem, ModelPickerTarget, ProviderFormSeed, ProviderListItem, ProviderOverlayAction,
};
use session_list::TuiSessionStore;
use session_picker::SessionPickerSelection;
use state::TuiState;
use std::collections::BTreeSet;
use terminal::TerminalSession;
use theme::TuiTheme;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[cfg(target_os = "linux")]
mod clipboard_image;
mod command;
mod command_controller;
mod completion;
mod controller;
mod highlight;
mod input;
mod input_history_store;
pub(crate) mod keymap;
mod layout;
mod markdown;
mod overlay;
mod overlay_render;
mod panels;
mod plan;
mod plan_controller;
mod plan_projector;
mod plan_render;
mod preferences;
mod projector;
mod provider_overlay;
mod provider_render;
mod render;
mod runtime;
mod session_list;
mod session_picker;
mod state;
mod status;
mod terminal;
pub(crate) mod theme;
mod tool_error;

#[cfg(test)]
mod history_tests;
#[cfg(test)]
mod plan_tests;
#[cfg(test)]
mod slash_stop_tests;
#[cfg(test)]
mod slash_tests;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchMode {
    New,
    ResumePicker,
}

pub(crate) async fn run(
    sandbox_child_handoff: Option<SandboxChildHandoff>,
    merry_config: Option<&MerryConfig>,
    launch_mode: LaunchMode,
    process_execution_mode: ProcessExecutionMode,
    fully_trusted: bool,
) -> Result<(), CliError> {
    let tui_config = merry_config
        .map(MerryConfig::tui_config)
        .transpose()
        .map_err(crate::cli_error::unexpected)?
        .unwrap_or_default();
    let keymap = Keymap::from_config(&tui_config.keymap).map_err(crate::cli_error::unexpected)?;
    let theme = TuiTheme::from_config(&tui_config.theme).map_err(crate::cli_error::unexpected)?;
    let paths = XdgPaths::from_env().map_err(crate::cli_error::unexpected)?;
    let mut provider_management =
        ProviderManagementService::new(paths.clone()).map_err(crate::cli_error::unexpected)?;
    let preferences_store = TuiPreferencesStore::new(paths.tui_preferences_file());
    let mut active_config = provider_management
        .config()
        .cloned()
        .or_else(|| merry_config.cloned());
    let mut settings_defaults = TuiSettingsDefaults::from_config(active_config.as_ref())
        .map_err(crate::cli_error::unexpected)?;
    let mut preferences = preferences_store
        .load_with_default_provider(settings_defaults.provider.as_deref())
        .await
        .map_err(crate::cli_error::unexpected)?;
    let workspace_root = std::env::current_dir().map_err(crate::cli_error::unexpected)?;
    let session_store = TuiSessionStore::new(
        merry_runtime::FileSessionStore::default_sessions_dir()
            .map_err(crate::cli_error::unexpected)?,
    );
    let mut terminal = TerminalSession::enter().map_err(crate::cli_error::unexpected)?;
    if !has_runtime_selection(active_config.as_ref(), &preferences) {
        let mut setup_state = TuiState::new(
            workspace_root.clone(),
            "Provider setup".to_owned(),
            keymap.clone(),
            theme.clone(),
        );
        setup_state.configure_preferences(preferences.clone(), settings_defaults.clone());
        let Some(configured) = run_provider_setup(
            &mut terminal,
            &mut setup_state,
            &mut provider_management,
            &preferences_store,
            preferences,
        )
        .await?
        else {
            return Ok(());
        };
        preferences = configured;
        active_config = provider_management.config().cloned();
        settings_defaults = TuiSettingsDefaults::from_config(active_config.as_ref())
            .map_err(crate::cli_error::unexpected)?;
    }
    let selection = match launch_mode {
        LaunchMode::New => SessionPickerSelection::New,
        LaunchMode::ResumePicker => {
            session_picker::pick_session(&mut terminal, &session_store, &workspace_root).await?
        }
    };
    if matches!(selection, SessionPickerSelection::Quit) {
        return Ok(());
    }
    let session = runtime::start_tui_runtime_session(
        sandbox_child_handoff,
        active_config.as_ref(),
        session_store,
        selection,
        process_execution_mode,
        fully_trusted,
        &preferences,
    )
    .await?;
    let mut state = TuiState::new(
        session.workspace_root.clone(),
        session.model_label.clone(),
        keymap,
        theme,
    );
    let input_history_store =
        InputHistoryStore::for_workspace(paths.state_dir(), &session.workspace_root);
    state.set_input_history(input_history_store.load().await);
    state.set_reasoning_effort_label(session.reasoning_effort_label.clone());
    state.set_completion_skills(session.skills.clone());
    state.configure_preferences(preferences, settings_defaults);
    if let Some(snapshot) = session.plan_snapshot().await? {
        state.plan_mut().update_snapshot(snapshot);
    }
    if session.resumed {
        let transcript = session.session_transcript().await?;
        if transcript.is_empty() {
            state.push_timeline_item(state::TimelineItem::Muted {
                title: "Resumed".to_owned(),
                detail: session.metadata.session_id.as_str().to_owned(),
            });
        } else {
            let mut projector = TuiProjector::default();
            for item in transcript {
                projector.apply_transcript_item(item, &mut state);
            }
        }
    }
    controller::run_controller(
        terminal,
        session,
        state,
        preferences_store,
        input_history_store,
        provider_management,
    )
    .await
}

fn has_runtime_selection(config: Option<&MerryConfig>, preferences: &TuiPreferences) -> bool {
    let Some(config) = config else {
        return false;
    };
    let provider = preferences
        .provider
        .as_deref()
        .map(str::to_owned)
        .or_else(|| {
            config
                .configured_default_provider()
                .ok()
                .flatten()
                .map(|provider| provider.alias)
        });
    let Some(provider) = provider else {
        return false;
    };
    preferences.model_for_provider(&provider).is_some()
        || config
            .provider_profile(&provider)
            .ok()
            .and_then(|profile| profile.default_model().cloned())
            .is_some()
}

struct SetupDiscovery {
    generation: u64,
    alias: String,
    result: Result<Vec<ModelListItem>, String>,
}

async fn run_provider_setup(
    terminal: &mut TerminalSession,
    state: &mut TuiState,
    provider_management: &mut ProviderManagementService,
    preferences_store: &TuiPreferencesStore,
    mut preferences: TuiPreferences,
) -> Result<Option<TuiPreferences>, CliError> {
    open_setup_provider_manager(state, provider_management)?;
    let (discovery_tx, mut discovery_rx) = mpsc::channel(2);
    let mut discovery_generation = 0_u64;
    let mut discovery_token: Option<CancellationToken> = None;

    loop {
        terminal
            .draw(|frame| render::render(frame, state))
            .map_err(crate::cli_error::unexpected)?;
        tokio::select! {
            event = terminal.next_event() => {
                let Some(event) = event.map_err(crate::cli_error::unexpected)? else {
                    return Ok(None);
                };
                match event {
                    terminal::TerminalEvent::Key(key) => {
                        if key.code == KeyCode::Char('q')
                            && key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                        {
                            return Ok(None);
                        }
                        let result = state
                            .overlay_mut()
                            .expect("provider setup always owns an overlay")
                            .handle_key(key);
                        let action = match result {
                            overlay::OverlayKeyResult::Close => {
                                return Ok(None);
                            }
                            overlay::OverlayKeyResult::Back
                                if matches!(state.overlay(), Some(overlay::Overlay::Dialog(_))) =>
                            {
                                state.back_overlay();
                                continue;
                            }
                            overlay::OverlayKeyResult::Back
                                if matches!(
                                    state.overlay(),
                                    Some(overlay::Overlay::ModelPicker(picker))
                                        if picker.target() == ModelPickerTarget::ProviderForm
                                ) =>
                            {
                                state.back_overlay();
                                continue;
                            }
                            overlay::OverlayKeyResult::Back => return Ok(None),
                            overlay::OverlayKeyResult::Provider(action) => action,
                            _ => ProviderOverlayAction::Consumed,
                        };
                        match action {
                            ProviderOverlayAction::Consumed | ProviderOverlayAction::Back => {}
                            ProviderOverlayAction::OpenProviderManager => {
                                cancel_setup_discovery(&mut discovery_token);
                                open_setup_provider_manager(state, provider_management)?;
                            }
                            ProviderOverlayAction::OpenProviderForm => {
                                cancel_setup_discovery(&mut discovery_token);
                                let used = provider_management
                                    .profiles()
                                    .map_err(crate::cli_error::unexpected)?
                                    .into_iter()
                                    .map(|profile| profile.alias().as_str().to_owned())
                                    .collect::<BTreeSet<_>>();
                                let alias = derive_provider_alias("Provider", &used)
                                    .map_err(crate::cli_error::unexpected)?;
                                state.open_provider_form(alias.as_str().to_owned(), used);
                            }
                            ProviderOverlayAction::OpenProviderEditor(alias) => {
                                cancel_setup_discovery(&mut discovery_token);
                                let alias = ProviderAlias::new(&alias)
                                    .map_err(crate::cli_error::unexpected)?;
                                let editable = match provider_management.editable_provider(&alias) {
                                    Ok(editable) => editable,
                                    Err(error) => {
                                        if matches!(
                                            &error,
                                            ProviderManagementError::ReadOnlyProvider { .. }
                                        ) {
                                            state.show_info_dialog(
                                                "Read-only provider",
                                                error.to_string(),
                                            );
                                        } else {
                                            state.set_provider_overlay_error(error.to_string());
                                        }
                                        continue;
                                    }
                                };
                                let used = provider_management
                                    .profiles()
                                    .map_err(crate::cli_error::unexpected)?
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
                                    },
                                    used,
                                );
                            }
                            ProviderOverlayAction::OpenModelPicker(alias)
                            | ProviderOverlayAction::RefreshModels(alias) => {
                                open_setup_model_picker(
                                    state,
                                    provider_management,
                                    &alias,
                                    &discovery_tx,
                                    &mut discovery_generation,
                                    &mut discovery_token,
                                )
                                .await?;
                            }
                            ProviderOverlayAction::BackToProviderForm => {
                                cancel_setup_discovery(&mut discovery_token);
                                state.back_overlay();
                            }
                            ProviderOverlayAction::DiscoverFormModels {
                                original_alias,
                                values,
                            } => {
                                cancel_setup_discovery(&mut discovery_token);
                                let draft = match controller::provider_discovery_draft(
                                    original_alias.as_deref(),
                                    &values,
                                ) {
                                    Ok(draft) => draft,
                                    Err(error) => {
                                        state.set_provider_overlay_error(error.to_string());
                                        continue;
                                    }
                                };
                                let alias = draft.alias().as_str().to_owned();
                                if !state.open_provider_form_model_picker(
                                    alias.clone(),
                                    values.display_name.clone(),
                                ) {
                                    state.set_provider_overlay_error(
                                        "provider form is no longer available for model discovery"
                                            .to_owned(),
                                    );
                                    continue;
                                }
                                start_setup_form_discovery(
                                    alias,
                                    draft,
                                    provider_management.clone(),
                                    &discovery_tx,
                                    &mut discovery_generation,
                                    &mut discovery_token,
                                );
                            }
                            ProviderOverlayAction::RefreshFormModels => {
                                let Some((original_alias, values)) =
                                    state.provider_form_discovery_request()
                                else {
                                    state.set_provider_overlay_error(
                                        "provider form is no longer available for model discovery"
                                            .to_owned(),
                                    );
                                    continue;
                                };
                                let draft = match controller::provider_discovery_draft(
                                    original_alias.as_deref(),
                                    &values,
                                ) {
                                    Ok(draft) => draft,
                                    Err(error) => {
                                        state.set_provider_overlay_error(error.to_string());
                                        continue;
                                    }
                                };
                                let alias = draft.alias().as_str().to_owned();
                                state.mark_model_picker_loading(&alias);
                                start_setup_form_discovery(
                                    alias,
                                    draft,
                                    provider_management.clone(),
                                    &discovery_tx,
                                    &mut discovery_generation,
                                    &mut discovery_token,
                                );
                            }
                            ProviderOverlayAction::DeleteProvider(alias) => {
                                let alias = ProviderAlias::new(&alias)
                                    .map_err(crate::cli_error::unexpected)?;
                                if let Err(error) = provider_management.delete_provider(&alias).await {
                                    state.set_provider_overlay_error(error.to_string());
                                    continue;
                                }
                                preferences
                                    .set_model_for_provider(alias.as_str(), None)
                                    .map_err(crate::cli_error::unexpected)?;
                                open_setup_provider_manager(state, provider_management)?;
                            }
                            ProviderOverlayAction::SaveProvider(values) => {
                                let alias = match ProviderAlias::new(values.alias.trim()) {
                                    Ok(alias) => alias,
                                    Err(error) => {
                                        state.set_provider_overlay_error(error.to_string());
                                        continue;
                                    }
                                };
                                let model = match merry_llm::ModelName::new(values.model.trim()) {
                                    Ok(model) => model,
                                    Err(error) => {
                                        state.set_provider_overlay_error(error.to_string());
                                        continue;
                                    }
                                };
                                let draft = match ProviderDraft::new(
                                    values.display_name.trim(),
                                    alias.clone(),
                                    values.kind,
                                    values.protocol,
                                    values.base_url.trim(),
                                    &values.api_key,
                                    model.clone(),
                                ) {
                                    Ok(draft) => draft,
                                    Err(error) => {
                                        state.set_provider_overlay_error(error.to_string());
                                        continue;
                                    }
                                };
                                if let Err(error) = provider_management.save_provider(draft).await {
                                    state.set_provider_overlay_error(error.to_string());
                                    continue;
                                }
                                preferences.provider = Some(alias.as_str().to_owned());
                                preferences
                                    .set_model_for_provider(alias.as_str(), Some(model.as_str()))
                                    .map_err(crate::cli_error::unexpected)?;
                                preferences_store
                                    .save(&preferences)
                                    .await
                                    .map_err(crate::cli_error::unexpected)?;
                                return Ok(Some(preferences));
                            }
                            ProviderOverlayAction::UpdateProvider {
                                original_alias,
                                values,
                            } => {
                                let original_alias = ProviderAlias::new(&original_alias)
                                    .map_err(crate::cli_error::unexpected)?;
                                let alias = match ProviderAlias::new(values.alias.trim()) {
                                    Ok(alias) => alias,
                                    Err(error) => {
                                        state.set_provider_overlay_error(error.to_string());
                                        continue;
                                    }
                                };
                                let model = match merry_llm::ModelName::new(values.model.trim()) {
                                    Ok(model) => model,
                                    Err(error) => {
                                        state.set_provider_overlay_error(error.to_string());
                                        continue;
                                    }
                                };
                                let api_key = (!values.api_key.trim().is_empty())
                                    .then(|| values.api_key.trim());
                                let draft = match ProviderDraft::for_update(
                                    values.display_name.trim(),
                                    alias.clone(),
                                    values.kind,
                                    values.protocol,
                                    values.base_url.trim(),
                                    api_key,
                                    model.clone(),
                                ) {
                                    Ok(draft) => draft,
                                    Err(error) => {
                                        state.set_provider_overlay_error(error.to_string());
                                        continue;
                                    }
                                };
                                if let Err(error) = provider_management
                                    .update_provider(&original_alias, draft)
                                    .await
                                {
                                    state.set_provider_overlay_error(error.to_string());
                                    continue;
                                }
                                preferences
                                    .set_model_for_provider(
                                        alias.as_str(),
                                        Some(model.as_str()),
                                    )
                                    .map_err(crate::cli_error::unexpected)?;
                                preferences_store
                                    .save(&preferences)
                                    .await
                                    .map_err(crate::cli_error::unexpected)?;
                                open_setup_provider_manager(state, provider_management)?;
                            }
                            ProviderOverlayAction::SelectModel {
                                alias,
                                model,
                                target: ModelPickerTarget::ActiveProvider,
                            } => {
                                let alias = ProviderAlias::new(&alias)
                                    .map_err(crate::cli_error::unexpected)?;
                                let model = merry_llm::ModelName::new(&model)
                                    .map_err(crate::cli_error::unexpected)?;
                                preferences.provider = Some(alias.as_str().to_owned());
                                preferences
                                    .set_model_for_provider(alias.as_str(), Some(model.as_str()))
                                    .map_err(crate::cli_error::unexpected)?;
                                preferences_store
                                    .save(&preferences)
                                    .await
                                    .map_err(crate::cli_error::unexpected)?;
                                cancel_setup_discovery(&mut discovery_token);
                                return Ok(Some(preferences));
                            }
                            ProviderOverlayAction::SelectModel {
                                model,
                                target: ModelPickerTarget::ProviderForm,
                                ..
                            } => {
                                cancel_setup_discovery(&mut discovery_token);
                                if !state.select_provider_form_model(&model) {
                                    state.set_provider_overlay_error(
                                        "provider form is no longer available for the selected model"
                                            .to_owned(),
                                    );
                                }
                            }
                        }
                    }
                    terminal::TerminalEvent::Paste(text) => {
                        state.insert_overlay_paste(&text);
                    }
                    terminal::TerminalEvent::Resize
                    | terminal::TerminalEvent::MouseScrollUp(_)
                    | terminal::TerminalEvent::MouseScrollDown(_) => {}
                }
            }
            completion = discovery_rx.recv() => {
                if let Some(completion) = completion
                    && completion.generation == discovery_generation
                {
                    state.update_model_picker(&completion.alias, completion.result);
                }
            }
        }
    }
}

fn open_setup_provider_manager(
    state: &mut TuiState,
    provider_management: &ProviderManagementService,
) -> Result<(), CliError> {
    let items = provider_management
        .profiles()
        .map_err(crate::cli_error::unexpected)?
        .into_iter()
        .map(|profile| {
            ProviderListItem::new(
                profile.alias().as_str(),
                profile.display_name(),
                profile.kind(),
                profile.source(),
                profile.protocol(),
                profile.default_model().map(merry_llm::ModelName::as_str),
            )
        })
        .collect::<Vec<_>>();
    if items.is_empty() {
        let alias = derive_provider_alias("Provider", &BTreeSet::new())
            .map_err(crate::cli_error::unexpected)?;
        state.open_provider_form(alias.as_str().to_owned(), BTreeSet::new());
    } else {
        state.open_provider_manager(items);
    }
    Ok(())
}

async fn open_setup_model_picker(
    state: &mut TuiState,
    provider_management: &ProviderManagementService,
    alias: &str,
    discovery_tx: &mpsc::Sender<SetupDiscovery>,
    discovery_generation: &mut u64,
    discovery_token: &mut Option<CancellationToken>,
) -> Result<(), CliError> {
    let alias = ProviderAlias::new(alias).map_err(crate::cli_error::unexpected)?;
    let profile = provider_management
        .config()
        .ok_or_else(|| crate::cli_error::unexpected("no providers are configured"))?
        .provider_profile(alias.as_str())
        .map_err(crate::cli_error::unexpected)?;
    let cached = provider_management
        .load_model_cache(&alias)
        .await
        .map_err(crate::cli_error::unexpected)?
        .map(setup_model_items)
        .unwrap_or_default();
    state.open_model_picker(
        alias.as_str().to_owned(),
        profile.display_name().to_owned(),
        cached,
    );
    cancel_setup_discovery(discovery_token);
    *discovery_generation = discovery_generation.wrapping_add(1);
    let generation = *discovery_generation;
    let token = CancellationToken::new();
    *discovery_token = Some(token.clone());
    let service = provider_management.clone();
    let sender = discovery_tx.clone();
    tokio::spawn(async move {
        let alias_text = alias.as_str().to_owned();
        let result = service
            .discover_and_cache(&alias, token)
            .await
            .map(setup_model_items)
            .map_err(|error| error.to_string());
        let _ = sender
            .send(SetupDiscovery {
                generation,
                alias: alias_text,
                result,
            })
            .await;
    });
    Ok(())
}

fn setup_model_items(catalog: merry_llm::ModelCatalog) -> Vec<ModelListItem> {
    catalog
        .into_models()
        .into_iter()
        .map(|model| ModelListItem::new(model.id().as_str(), model.owner()))
        .collect()
}

fn start_setup_form_discovery(
    alias: String,
    draft: ProviderDiscoveryDraft,
    provider_management: ProviderManagementService,
    discovery_tx: &mpsc::Sender<SetupDiscovery>,
    discovery_generation: &mut u64,
    discovery_token: &mut Option<CancellationToken>,
) {
    cancel_setup_discovery(discovery_token);
    *discovery_generation = discovery_generation.wrapping_add(1);
    let generation = *discovery_generation;
    let token = CancellationToken::new();
    *discovery_token = Some(token.clone());
    let sender = discovery_tx.clone();
    tokio::spawn(async move {
        let result = provider_management
            .discover_from_draft(draft, token)
            .await
            .map(setup_model_items)
            .map_err(|error| error.to_string());
        let _ = sender
            .send(SetupDiscovery {
                generation,
                alias,
                result,
            })
            .await;
    });
}

fn cancel_setup_discovery(token: &mut Option<CancellationToken>) {
    if let Some(token) = token.take() {
        token.cancel();
    }
}

#[cfg(test)]
mod provider_setup_tests {
    use super::*;
    use crate::config::XdgPaths;

    #[test]
    fn provider_setup_is_required_only_until_provider_and_model_resolve() {
        let paths = XdgPaths::from_parts(std::path::PathBuf::from("/home/alice"), None, None);
        let config = MerryConfig::load_optional_from_text(
            Some(
                r#"
[providers.opencode]
display_name = "OpenCode"
default_model = "deepseek-v4-pro"
type = "openai-compatible"
api_key = "sk-test"
"#,
            ),
            &paths,
        )
        .expect("config should parse")
        .expect("config should exist");
        let mut preferences = TuiPreferences::default();

        assert!(!has_runtime_selection(Some(&config), &preferences));
        preferences.provider = Some("opencode".to_owned());
        assert!(has_runtime_selection(Some(&config), &preferences));
        preferences
            .set_model_for_provider("opencode", Some("manual-model"))
            .expect("manual model");
        assert!(has_runtime_selection(Some(&config), &preferences));
    }
}
