use super::{
    input::{DraftImage, TuiSubmission},
    keymap::KeyAction,
    layout::{BottomPaneHeights, cockpit_layout},
    overlay::{OverlayKeyResult, PaletteCommand},
    preferences::{TuiPreferences, TuiPreferencesStore, TuiSettingsDefaults},
    projector::TuiProjector,
    provider_overlay::{
        ModelListItem, ModelPickerTarget, ProviderFormSeed, ProviderFormValues, ProviderListItem,
        ProviderOverlayAction,
    },
    render,
    runtime::TuiRuntimeSession,
    state::TuiState,
    terminal::{TerminalEvent, TerminalSession},
};
use crate::{
    cli_error::{CliError, unexpected},
    config::{ProviderAlias, derive_provider_alias},
    provider_management::{
        ProviderDiscoveryDraft, ProviderDraft, ProviderManagementError, ProviderManagementService,
    },
};
use crossterm::event::{KeyCode, KeyEvent};
use futures_util::StreamExt;
use merry_core::QueuedInputLane;
use merry_runtime::InterruptReason;
use ratatui::layout::{Position, Rect, Size};
use std::{collections::BTreeSet, time::Duration};
use tokio::{sync::mpsc, time};
use tokio_util::sync::CancellationToken;

const TIMELINE_SCROLL_STEP: usize = 5;
const FOCUS_SCROLL_STEP: usize = 5;
const PLAN_SCROLL_STEP: usize = 5;

struct ModelDiscoveryCompletion {
    generation: u64,
    alias: String,
    result: Result<Vec<ModelListItem>, String>,
}

struct ClipboardImageCompletion {
    result: Result<DraftImage, String>,
}

struct ProviderController<'a> {
    management: &'a mut ProviderManagementService,
    discovery_tx: &'a mpsc::Sender<ModelDiscoveryCompletion>,
    discovery_generation: &'a mut u64,
    discovery_token: &'a mut Option<CancellationToken>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControllerEffect {
    None,
    SubmitNext(TuiSubmission),
    SubmitBacklog(TuiSubmission),
    PasteImage,
    Interrupt,
    ResumeSuspended,
    DiscardSuspended,
    PersistPreferences(TuiPreferences),
    ApplyRuntimePreferences(TuiPreferences),
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
    SelectProviderModel {
        alias: String,
        model: String,
    },
    SelectProviderFormModel(String),
    EnterPlanMode,
    ApprovePlan(merry_runtime::PlanApprovalInput),
    ApprovePermission(String),
    DenyPermission(String),
    RevisePlan,
    RetryPlanNode(merry_core::PlanNodeId),
    CancelPlan,
    Quit,
}

pub(crate) fn handle_key_action(action: KeyAction, state: &mut TuiState) -> ControllerEffect {
    match action {
        KeyAction::SubmitNext => {
            if exit_review_if_active(state) {
                return ControllerEffect::None;
            }
            state
                .take_input_for_submit()
                .map_or(ControllerEffect::None, ControllerEffect::SubmitNext)
        }
        KeyAction::SubmitBacklog => {
            if exit_review_if_active(state) {
                return ControllerEffect::None;
            }
            state
                .take_input_for_submit()
                .map_or(ControllerEffect::None, ControllerEffect::SubmitBacklog)
        }
        KeyAction::CancelInputOrQuit => {
            if state.cancel_input_or_mark_quit() {
                ControllerEffect::Quit
            } else {
                ControllerEffect::None
            }
        }
        KeyAction::InsertNewline => {
            state.insert_input_newline();
            ControllerEffect::None
        }
        KeyAction::PasteImage => ControllerEffect::PasteImage,
        KeyAction::OpenCommandPanel => {
            state.open_command_palette();
            ControllerEffect::None
        }
        KeyAction::Interrupt => {
            if state.is_active_run() {
                ControllerEffect::Interrupt
            } else if state.is_artifact_reviewing() {
                state.exit_artifact_review();
                ControllerEffect::None
            } else if state.is_timeline_reviewing() {
                state.exit_timeline_review();
                ControllerEffect::None
            } else {
                ControllerEffect::None
            }
        }
        KeyAction::Quit => ControllerEffect::Quit,
        KeyAction::ScrollUp => {
            state.scroll_timeline_up_by(TIMELINE_SCROLL_STEP);
            ControllerEffect::None
        }
        KeyAction::ScrollDown => {
            state.scroll_timeline_down_by(TIMELINE_SCROLL_STEP);
            ControllerEffect::None
        }
        KeyAction::ReviewPreviousUserInput => {
            state.jump_to_previous_user_input();
            ControllerEffect::None
        }
        KeyAction::ReviewPreviousArtifact => {
            state.select_previous_artifact();
            ControllerEffect::None
        }
        KeyAction::ReviewNextArtifact => {
            state.select_next_artifact();
            ControllerEffect::None
        }
        KeyAction::FollowLatestArtifact => {
            state.follow_latest();
            ControllerEffect::None
        }
        KeyAction::HistoryPrevious => {
            state.previous_input_history();
            ControllerEffect::None
        }
        KeyAction::HistoryNext => {
            state.next_input_history();
            ControllerEffect::None
        }
        KeyAction::ResumeSuspended => ControllerEffect::ResumeSuspended,
        KeyAction::DiscardSuspended => ControllerEffect::DiscardSuspended,
        _ => ControllerEffect::None,
    }
}

fn exit_review_if_active(state: &mut TuiState) -> bool {
    let was_reviewing = state.is_timeline_reviewing() || state.is_artifact_reviewing();
    if state.is_timeline_reviewing() {
        state.exit_timeline_review();
    }
    if state.is_artifact_reviewing() {
        state.exit_artifact_review();
    }
    was_reviewing
}

pub(crate) fn handle_key_event(key: KeyEvent, state: &mut TuiState) -> ControllerEffect {
    if state.overlay().is_some() {
        let result = state
            .overlay_mut()
            .expect("overlay checked above")
            .handle_key(key);
        return match result {
            OverlayKeyResult::Consumed => ControllerEffect::None,
            OverlayKeyResult::Close => {
                state.close_overlay();
                ControllerEffect::None
            }
            OverlayKeyResult::Back => {
                state.back_overlay();
                ControllerEffect::None
            }
            OverlayKeyResult::Run(command) => run_palette_command(command, state),
            OverlayKeyResult::AdjustSetting(item, direction) => {
                if state.adjust_setting(item, direction) {
                    preferences_effect(item, state.preferences().clone())
                } else {
                    ControllerEffect::None
                }
            }
            OverlayKeyResult::ResetSetting(item) => {
                if state.reset_setting(item) {
                    preferences_effect(item, state.preferences().clone())
                } else {
                    ControllerEffect::None
                }
            }
            OverlayKeyResult::BeginModelEdit => {
                state.begin_settings_model_edit();
                ControllerEffect::None
            }
            OverlayKeyResult::CommitModel(value) => {
                if state.commit_settings_model(value) {
                    ControllerEffect::ApplyRuntimePreferences(state.preferences().clone())
                } else {
                    ControllerEffect::None
                }
            }
            OverlayKeyResult::BeginContextWindowEdit => {
                state.begin_settings_context_window_edit();
                ControllerEffect::None
            }
            OverlayKeyResult::CommitContextWindow(value) => {
                if state.commit_settings_context_window(value) {
                    ControllerEffect::ApplyRuntimePreferences(state.preferences().clone())
                } else {
                    ControllerEffect::None
                }
            }
            OverlayKeyResult::OpenShortcuts => {
                state.open_shortcuts();
                ControllerEffect::None
            }
            OverlayKeyResult::ConfirmPlanApproval => {
                let Some(input) = state.plan_approval_input() else {
                    state.close_overlay();
                    return ControllerEffect::None;
                };
                state.close_overlay();
                ControllerEffect::ApprovePlan(input)
            }
            OverlayKeyResult::ApprovePermission(approval_id) => {
                state.close_overlay();
                ControllerEffect::ApprovePermission(approval_id)
            }
            OverlayKeyResult::DenyPermission(approval_id) => {
                state.close_overlay();
                ControllerEffect::DenyPermission(approval_id)
            }
            OverlayKeyResult::Provider(action) => provider_overlay_effect(action),
        };
    }

    if state.completion_menu().is_some() {
        match key.code {
            KeyCode::Enter | KeyCode::Tab => {
                state.accept_completion();
                return ControllerEffect::None;
            }
            KeyCode::Down => {
                state.select_next_completion();
                return ControllerEffect::None;
            }
            KeyCode::Up => {
                state.select_previous_completion();
                return ControllerEffect::None;
            }
            KeyCode::Esc => {
                state.close_completion_menu();
                return ControllerEffect::None;
            }
            _ => {}
        }
    }

    if state.plan().is_focused() {
        if super::plan_controller::handle_navigation_key(key, state) {
            return ControllerEffect::None;
        }
        if let Some(action) = state.keymap().action_for(key.into())
            && matches!(
                action,
                KeyAction::OpenCommandPanel | KeyAction::Interrupt | KeyAction::Quit
            )
        {
            return handle_key_action(action, state);
        }
        return ControllerEffect::None;
    }

    if let Some(action) = state.keymap().action_for(key.into()) {
        return handle_key_action(action, state);
    }
    state.handle_input_key(key);
    ControllerEffect::None
}

fn provider_overlay_effect(action: ProviderOverlayAction) -> ControllerEffect {
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
        ProviderOverlayAction::SelectModel {
            alias,
            model,
            target: ModelPickerTarget::ActiveProvider,
        } => ControllerEffect::SelectProviderModel { alias, model },
        ProviderOverlayAction::SelectModel {
            model,
            target: ModelPickerTarget::ProviderForm,
            ..
        } => ControllerEffect::SelectProviderFormModel(model),
    }
}

fn preferences_effect(
    item: super::overlay::SettingItem,
    preferences: TuiPreferences,
) -> ControllerEffect {
    match item {
        super::overlay::SettingItem::CodeTheme => ControllerEffect::PersistPreferences(preferences),
        super::overlay::SettingItem::KeyboardShortcuts => ControllerEffect::None,
        _ => ControllerEffect::ApplyRuntimePreferences(preferences),
    }
}

pub(crate) fn handle_paste_event(text: &str, state: &mut TuiState) {
    if !state.insert_overlay_paste(text) {
        state.insert_input_paste(text);
    }
}

fn run_palette_command(command: PaletteCommand, state: &mut TuiState) -> ControllerEffect {
    if let Some(effect) = super::plan_controller::palette_effect(command, state) {
        return effect;
    }
    match command {
        PaletteCommand::OpenSettings => {
            state.close_overlay();
            state.open_settings();
            ControllerEffect::None
        }
        PaletteCommand::OpenProviders => ControllerEffect::OpenProviderManager,
        PaletteCommand::ShowShortcuts => {
            state.open_shortcuts();
            ControllerEffect::None
        }
        PaletteCommand::FollowLatest => {
            state.close_overlay();
            handle_key_action(KeyAction::FollowLatestArtifact, state)
        }
        PaletteCommand::ReviewPreviousArtifact => {
            state.close_overlay();
            handle_key_action(KeyAction::ReviewPreviousArtifact, state)
        }
        PaletteCommand::ReviewNextArtifact => {
            state.close_overlay();
            handle_key_action(KeyAction::ReviewNextArtifact, state)
        }
        PaletteCommand::ReviewPreviousUserInput => {
            state.close_overlay();
            handle_key_action(KeyAction::ReviewPreviousUserInput, state)
        }
        PaletteCommand::Interrupt => {
            state.close_overlay();
            handle_key_action(KeyAction::Interrupt, state)
        }
        PaletteCommand::ResumeSuspended => {
            state.close_overlay();
            handle_key_action(KeyAction::ResumeSuspended, state)
        }
        PaletteCommand::DiscardSuspended => {
            state.close_overlay();
            handle_key_action(KeyAction::DiscardSuspended, state)
        }
        PaletteCommand::EnterPlanMode
        | PaletteCommand::ApprovePlan
        | PaletteCommand::RevisePlan
        | PaletteCommand::OpenPlan
        | PaletteCommand::FocusPlan
        | PaletteCommand::ClosePlan
        | PaletteCommand::RetryPlanNode
        | PaletteCommand::CancelPlan => unreachable!("plan command handled above"),
        PaletteCommand::Quit => {
            state.close_overlay();
            ControllerEffect::Quit
        }
    }
}

pub(crate) fn handle_mouse_scroll_up(
    position: Position,
    terminal_size: Size,
    state: &mut TuiState,
) {
    if state.overlay().is_some() {
        return;
    }
    if position_in_plan_pane(position, terminal_size, state) {
        if state.plan().is_inspector_open() {
            state.plan_mut().scroll_inspector_up_by(PLAN_SCROLL_STEP);
        } else {
            state.plan_mut().scroll_up_by(PLAN_SCROLL_STEP);
        }
    } else if position_in_focus_pane(position, terminal_size, state) {
        state.scroll_focus_up_by(FOCUS_SCROLL_STEP);
    } else {
        state.scroll_timeline_up_by(TIMELINE_SCROLL_STEP);
    }
}

pub(crate) fn handle_mouse_scroll_down(
    position: Position,
    terminal_size: Size,
    state: &mut TuiState,
) {
    if state.overlay().is_some() {
        return;
    }
    if position_in_plan_pane(position, terminal_size, state) {
        if state.plan().is_inspector_open() {
            state.plan_mut().scroll_inspector_down_by(PLAN_SCROLL_STEP);
        } else {
            state.plan_mut().scroll_down_by(PLAN_SCROLL_STEP);
        }
    } else if position_in_focus_pane(position, terminal_size, state) {
        state.scroll_focus_down_by(FOCUS_SCROLL_STEP);
    } else {
        state.scroll_timeline_down_by(TIMELINE_SCROLL_STEP);
    }
}

fn position_in_focus_pane(position: Position, terminal_size: Size, state: &TuiState) -> bool {
    cockpit_rects(terminal_size, state)
        .detail
        .is_some_and(|detail| detail.contains(position))
}

fn position_in_plan_pane(position: Position, terminal_size: Size, state: &TuiState) -> bool {
    cockpit_rects(terminal_size, state)
        .plan
        .is_some_and(|plan| plan.contains(position))
}

fn cockpit_rects(terminal_size: Size, state: &TuiState) -> super::layout::TimelineRects {
    let area = Rect::new(0, 0, terminal_size.width, terminal_size.height);
    let pane_heights = render::pane_heights_for_area(state, area);
    cockpit_layout(
        area,
        BottomPaneHeights {
            queue: pane_heights.queue,
            completion: pane_heights.completion,
            input: pane_heights.input,
            status: render::STATUS_HEIGHT,
        },
        state.is_artifact_reviewing(),
        state.plan().is_open(),
        state.plan().is_focused(),
    )
}

pub(crate) async fn run_controller(
    mut terminal: TerminalSession,
    mut session: TuiRuntimeSession,
    mut state: TuiState,
    preferences_store: TuiPreferencesStore,
    mut provider_management: ProviderManagementService,
) -> Result<(), CliError> {
    let mut projector = TuiProjector::default();
    let (model_discovery_tx, mut model_discovery_rx) = mpsc::channel(4);
    let (clipboard_image_tx, mut clipboard_image_rx) = mpsc::channel(4);
    let mut model_discovery_generation = 0_u64;
    let mut model_discovery_token: Option<CancellationToken> = None;
    render_once(&mut terminal, &state)?;

    loop {
        tokio::select! {
            _ = time::sleep(Duration::from_millis(100)), if state.is_active_run() => {
                if let Some(next) = session.prune_cancelled_permission_reviews() {
                    match next {
                        Some((approval_id, body)) => state.open_permission_review(approval_id, body),
                        None => state.close_overlay(),
                    }
                }
                render_once(&mut terminal, &state)?;
            }
            event = terminal.next_event() => {
                let Some(event) = event.map_err(unexpected)? else {
                    break;
                };

                match event {
                    TerminalEvent::Key(key) => {
                        let effect = handle_key_event(key, &mut state);
                        project_local_effect(&effect, &mut state);
                        render_once(&mut terminal, &state)?;
                        let mut providers = ProviderController {
                            management: &mut provider_management,
                            discovery_tx: &model_discovery_tx,
                            discovery_generation: &mut model_discovery_generation,
                            discovery_token: &mut model_discovery_token,
                        };
                        let should_quit = dispatch_effect(
                            effect,
                            &mut session,
                            &mut state,
                            &preferences_store,
                            &mut providers,
                            &clipboard_image_tx,
                        )
                        .await?;
                        if should_quit {
                            break;
                        }
                        render_once(&mut terminal, &state)?;
                    }
                    TerminalEvent::MouseScrollUp(position) => {
                        let size = terminal.size().map_err(unexpected)?;
                        handle_mouse_scroll_up(position, size, &mut state);
                        render_once(&mut terminal, &state)?;
                    }
                    TerminalEvent::MouseScrollDown(position) => {
                        let size = terminal.size().map_err(unexpected)?;
                        handle_mouse_scroll_down(position, size, &mut state);
                        render_once(&mut terminal, &state)?;
                    }
                    TerminalEvent::Paste(text) => {
                        handle_paste_event(&text, &mut state);
                        render_once(&mut terminal, &state)?;
                    }
                    TerminalEvent::Resize => {
                        render_once(&mut terminal, &state)?;
                    }
                }
            }
            event = session.stream.next() => {
                let Some(event) = event else {
                    break;
                };
                projector.apply(event, &mut state);
                render_once(&mut terminal, &state)?;
            }
            request = session.permission_requests.recv() => {
                let Some(request) = request else {
                    continue;
                };
                if let Some((approval_id, body)) = session.enqueue_permission_review(request) {
                    state.open_permission_review(approval_id, body);
                }
                render_once(&mut terminal, &state)?;
            }
            completion = model_discovery_rx.recv() => {
                let Some(completion) = completion else {
                    continue;
                };
                if completion.generation == model_discovery_generation {
                    state.update_model_picker(&completion.alias, completion.result);
                    render_once(&mut terminal, &state)?;
                }
            }
            completion = clipboard_image_rx.recv() => {
                let Some(completion) = completion else {
                    continue;
                };
                apply_clipboard_image_completion(completion.result, &mut state);
                render_once(&mut terminal, &state)?;
            }
        }
    }

    Ok(())
}

async fn dispatch_effect(
    effect: ControllerEffect,
    session: &mut TuiRuntimeSession,
    state: &mut TuiState,
    preferences_store: &TuiPreferencesStore,
    providers: &mut ProviderController<'_>,
    clipboard_image_tx: &mpsc::Sender<ClipboardImageCompletion>,
) -> Result<bool, CliError> {
    if let Some(should_quit) =
        super::plan_controller::dispatch_effect(&effect, session, state).await
    {
        return Ok(should_quit);
    }
    match effect {
        ControllerEffect::None => Ok(false),
        ControllerEffect::SubmitNext(submission) => {
            let message = submission.into_user_message().map_err(unexpected)?;
            session
                .input
                .submit_next_message(message)
                .await
                .map_err(unexpected)?;
            Ok(false)
        }
        ControllerEffect::SubmitBacklog(submission) => {
            let message = submission.into_user_message().map_err(unexpected)?;
            session
                .input
                .enqueue_message(message)
                .await
                .map_err(unexpected)?;
            Ok(false)
        }
        ControllerEffect::PasteImage => {
            start_clipboard_image_read(clipboard_image_tx.clone());
            Ok(false)
        }
        ControllerEffect::ApprovePermission(approval_id) => {
            if let Some(next) = session.resolve_permission_review(&approval_id, true)? {
                state.open_permission_review(next.0, next.1);
            }
            Ok(false)
        }
        ControllerEffect::DenyPermission(approval_id) => {
            if let Some(next) = session.resolve_permission_review(&approval_id, false)? {
                state.open_permission_review(next.0, next.1);
            }
            Ok(false)
        }
        ControllerEffect::Interrupt => {
            session
                .control
                .interrupt(InterruptReason::User)
                .await
                .map_err(unexpected)?;
            Ok(false)
        }
        ControllerEffect::ResumeSuspended => {
            session
                .control
                .resume_suspended()
                .await
                .map_err(unexpected)?;
            Ok(false)
        }
        ControllerEffect::DiscardSuspended => {
            session
                .control
                .discard_suspended()
                .await
                .map_err(unexpected)?;
            Ok(false)
        }
        ControllerEffect::PersistPreferences(preferences) => {
            preferences_store
                .save(&preferences)
                .await
                .map_err(unexpected)?;
            Ok(false)
        }
        ControllerEffect::ApplyRuntimePreferences(preferences) => {
            session.apply_preferences(&preferences).await?;
            preferences_store
                .save(&preferences)
                .await
                .map_err(unexpected)?;
            state.set_model_label(session.model_label.clone());
            state.set_reasoning_effort_label(session.reasoning_effort_label.clone());
            Ok(false)
        }
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
                .set_model_for_provider(alias.as_str(), None)
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
            let alias = match ProviderAlias::new(values.alias.trim()) {
                Ok(alias) => alias,
                Err(error) => {
                    state.set_provider_overlay_error(error.to_string());
                    return Ok(false);
                }
            };
            let model = match merry_llm::ModelName::new(values.model.trim()) {
                Ok(model) => model,
                Err(error) => {
                    state.set_provider_overlay_error(error.to_string());
                    return Ok(false);
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
                    return Ok(false);
                }
            };
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
            let alias = match ProviderAlias::new(values.alias.trim()) {
                Ok(alias) => alias,
                Err(error) => {
                    state.set_provider_overlay_error(error.to_string());
                    return Ok(false);
                }
            };
            let model = match merry_llm::ModelName::new(values.model.trim()) {
                Ok(model) => model,
                Err(error) => {
                    state.set_provider_overlay_error(error.to_string());
                    return Ok(false);
                }
            };
            let api_key = (!values.api_key.trim().is_empty()).then(|| values.api_key.trim());
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
            let mut preferences = state.preferences().clone();
            preferences
                .set_model_for_provider(alias.as_str(), Some(model.as_str()))
                .map_err(unexpected)?;
            if let Err(error) = session.apply_preferences(&preferences).await {
                state.set_provider_overlay_error(format!(
                    "provider updated; runtime refresh failed: {error:?}"
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
        ControllerEffect::SelectProviderModel { alias, model } => {
            let alias = ProviderAlias::new(&alias).map_err(unexpected)?;
            let model = merry_llm::ModelName::new(&model).map_err(unexpected)?;
            let mut preferences = state.preferences().clone();
            preferences.provider = Some(alias.as_str().to_owned());
            preferences
                .set_model_for_provider(alias.as_str(), Some(model.as_str()))
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
            cancel_model_discovery(providers.discovery_token);
            let items = provider_list_items(providers.management, state)?;
            state.open_provider_manager(items);
            Ok(false)
        }
        ControllerEffect::SelectProviderFormModel(model) => {
            cancel_model_discovery(providers.discovery_token);
            if !state.select_provider_form_model(&model) {
                state.set_provider_overlay_error(
                    "provider form is no longer available for the selected model".to_owned(),
                );
            }
            Ok(false)
        }
        ControllerEffect::EnterPlanMode
        | ControllerEffect::ApprovePlan(_)
        | ControllerEffect::RevisePlan
        | ControllerEffect::RetryPlanNode(_)
        | ControllerEffect::CancelPlan => unreachable!("plan effect handled above"),
        ControllerEffect::Quit => {
            session.set_title(state.latest_user_input_title());
            session.control.close().await.map_err(unexpected)?;
            session.stream.wait_until_closed().await;
            session.save_on_exit().await?;
            Ok(true)
        }
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

fn project_local_effect(effect: &ControllerEffect, state: &mut TuiState) {
    match effect {
        ControllerEffect::SubmitNext(submission) => {
            state.push_local_user_echo(submission.text.clone(), QueuedInputLane::Next);
        }
        ControllerEffect::SubmitBacklog(submission) => {
            state.push_local_user_echo(submission.text.clone(), QueuedInputLane::Backlog);
        }
        ControllerEffect::PersistPreferences(_)
        | ControllerEffect::ApplyRuntimePreferences(_)
        | ControllerEffect::OpenProviderManager
        | ControllerEffect::OpenProviderForm
        | ControllerEffect::OpenProviderEditor(_)
        | ControllerEffect::OpenModelPicker(_)
        | ControllerEffect::SaveProvider(_)
        | ControllerEffect::UpdateProvider { .. }
        | ControllerEffect::RefreshModels(_)
        | ControllerEffect::DeleteProvider(_)
        | ControllerEffect::SelectProviderModel { .. } => {}
        _ => {}
    }
}

#[cfg(target_os = "linux")]
fn start_clipboard_image_read(sender: mpsc::Sender<ClipboardImageCompletion>) {
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(|| {
            super::clipboard_image::read_clipboard_image()
                .map_err(|error| error.to_string())
                .and_then(|image| image.into_draft_image().map_err(|error| error.to_string()))
        })
        .await
        .unwrap_or_else(|error| Err(format!("clipboard image task failed: {error}")));
        let _ = sender.send(ClipboardImageCompletion { result }).await;
    });
}

#[cfg(not(target_os = "linux"))]
fn start_clipboard_image_read(sender: mpsc::Sender<ClipboardImageCompletion>) {
    tokio::spawn(async move {
        let _ =
            sender
                .send(ClipboardImageCompletion {
                    result: Err(
                        "clipboard image paste is currently supported only on Linux".to_owned()
                    ),
                })
                .await;
    });
}

pub(crate) fn apply_clipboard_image_completion(
    result: Result<DraftImage, String>,
    state: &mut TuiState,
) {
    let result = result.and_then(|image| {
        state
            .insert_input_image(image)
            .map_err(|error| error.to_string())
    });
    if let Err(error) = result {
        state.push_timeline_item(super::state::TimelineItem::Diagnostic {
            title: "clipboard_image".to_owned(),
            body: error,
        });
    }
}

fn render_once(terminal: &mut TerminalSession, state: &TuiState) -> Result<(), CliError> {
    terminal
        .draw(|frame| render::render(frame, state))
        .map_err(unexpected)?;
    Ok(())
}
