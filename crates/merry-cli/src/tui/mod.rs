use crate::cli_error::CliError;
use crate::coding::{AutoFallback, PermissionMode, SandboxMode};
use crate::config::{MerryConfig, XdgPaths};
use crate::provider_management::ProviderManagementService;
use crate::workspace_trust::{self, TrustWorkspace, WorkspaceLaunch};

use input_history_store::InputHistoryStore;
use keymap::Keymap;
use preferences::{TuiPreferences, TuiPreferencesStore, TuiSettingsDefaults};
use projector::TuiProjector;
use session_list::TuiSessionStore;
use session_picker::SessionPickerSelection;
use state::TuiState;
use terminal::TerminalSession;
use theme::TuiTheme;

mod click_target;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod clipboard_image;
mod command;
mod command_controller;
mod command_details;
mod completion;
mod controller;
mod controller_provider;
mod copy_controls;
mod fuzzy;
pub(crate) mod glyphs;
mod highlight;
mod input;
mod input_history_store;
mod input_layout;
pub(crate) mod keymap;
mod layout;
mod links;
mod markdown;
mod modal_frame;
mod motion;
mod output_rate;
mod overlay;
mod overlay_render;
mod permission_render;
mod plan_controller;
mod plan_view;
pub(crate) mod preferences;
mod process_output;
mod projector;
mod provider_overlay;
mod provider_render;
mod provider_selection;
mod provider_setup;
mod reasoning_picker;
mod render;
mod runtime;
pub(crate) mod session_list;
mod session_picker;
mod settings_overlay;
mod settings_render;
mod skills_overlay;
mod state;
mod status;
mod subagent_view;
mod task_dock;
mod tasks;
mod terminal;
mod text_interaction;
mod text_sizing;
mod text_wrap;
pub(crate) mod theme;
mod todo_dock;
mod tool_error;
mod transcript;
pub(crate) mod trust_dialog;

#[cfg(test)]
mod history_tests;
#[cfg(test)]
mod mcp_tests;
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

async fn open_workspace(
    terminal: &mut TerminalSession,
    launch: WorkspaceLaunch,
    theme: &TuiTheme,
) -> Result<Option<workspace_trust::ResolvedWorkspace>, CliError> {
    match launch {
        WorkspaceLaunch::Ready(workspace) => Ok(Some(workspace)),
        WorkspaceLaunch::Ask(pending) => {
            let choice = trust_dialog::ask(terminal, pending.root(), theme).await?;
            pending.complete(choice)
        }
    }
}

pub(crate) async fn run(
    merry_config: Option<&MerryConfig>,
    launch_mode: LaunchMode,
    sandbox_mode: SandboxMode,
    permission_mode: PermissionMode,
    auto_fallback: AutoFallback,
    explicit_flags: crate::cli::ExplicitCliFlags,
    trust_workspace: Option<TrustWorkspace>,
) -> Result<(), CliError> {
    let tui_config = merry_config
        .map(MerryConfig::tui_config)
        .transpose()
        .map_err(crate::cli_error::unexpected)?
        .unwrap_or_default();
    let keymap = Keymap::from_config(&tui_config.keymap).map_err(crate::cli_error::unexpected)?;
    let configured_theme =
        TuiTheme::from_config_opaque(&tui_config.theme).map_err(crate::cli_error::unexpected)?;
    let paths = XdgPaths::from_env().map_err(crate::cli_error::unexpected)?;
    let mut provider_management =
        ProviderManagementService::new(paths.clone()).map_err(crate::cli_error::unexpected)?;
    let preferences_store = TuiPreferencesStore::new(paths.tui_preferences_file());
    let mut active_config = provider_management
        .config()
        .cloned()
        .or_else(|| merry_config.cloned());
    let mut settings_defaults = TuiSettingsDefaults::from_config(active_config.as_ref())
        .map_err(crate::cli_error::unexpected)?
        .with_explicit_cli_flags(explicit_flags);
    let mut preferences = preferences_store
        .load_with_default_provider(settings_defaults.provider.as_deref())
        .await
        .map_err(crate::cli_error::unexpected)?;
    let startup_sandbox = resolve_effective_sandbox(explicit_flags, &preferences, sandbox_mode);
    if startup_sandbox == SandboxMode::Workspace
        && sandbox_mode != SandboxMode::Workspace
        && let Err(error) = merry_process::probe_workspace_sandbox()
    {
        return Err(crate::cli_error::unexpected(format!(
            "{error}\nSee SANDBOX.md (https://github.com/locez/merry/blob/main/SANDBOX.md)."
        )));
    }
    let startup_transparent = preferences
        .transparent_background
        .unwrap_or(settings_defaults.transparent_background);
    let startup_theme = preferences
        .theme_preset
        .map(TuiTheme::from_preset)
        .unwrap_or_else(|| configured_theme.clone())
        .with_transparent_background(startup_transparent);
    let launch =
        workspace_trust::prepare_product_workspace(startup_sandbox, trust_workspace, &paths)?;
    let mut terminal = match TerminalSession::enter() {
        Ok(terminal) => terminal,
        Err(error) => {
            return Err(match &launch {
                WorkspaceLaunch::Ask(pending) => {
                    workspace_trust::prompt_unavailable(pending.root())
                }
                WorkspaceLaunch::Ready(_) => crate::cli_error::unexpected(error),
            });
        }
    };
    let Some(workspace) = open_workspace(&mut terminal, launch, &startup_theme).await? else {
        return Ok(());
    };
    let workspace_root = workspace.root;
    let session_store = TuiSessionStore::new(
        merry_runtime::FileSessionStore::default_sessions_dir()
            .map_err(crate::cli_error::unexpected)?,
    );
    if !provider_setup::has_runtime_selection(active_config.as_ref(), &preferences) {
        let mut setup_state = TuiState::new(
            workspace_root.clone(),
            "Provider setup".to_owned(),
            keymap.clone(),
            configured_theme.clone(),
        );
        setup_state.configure_preferences(preferences.clone(), settings_defaults.clone());
        let Some(configured) = provider_setup::run_provider_setup(
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
            .map_err(crate::cli_error::unexpected)?
            .with_explicit_cli_flags(explicit_flags);
    }
    let input_history_store = InputHistoryStore::for_workspace(paths.state_dir(), &workspace_root);
    let mut selection = match launch_mode {
        LaunchMode::New => SessionPickerSelection::New,
        LaunchMode::ResumePicker => {
            session_picker::pick_session(
                &mut terminal,
                &session_store,
                &workspace_root,
                &startup_theme,
            )
            .await?
        }
    };
    let mut announce_new_session = false;
    let mut pending_resume_hint: Option<(runtime::ResumeFailureKind, String, std::path::PathBuf)> =
        None;
    let mut current_picker_theme = startup_theme;
    loop {
        if matches!(selection, SessionPickerSelection::Quit) {
            return Ok(());
        }
        let effective_sandbox =
            resolve_effective_sandbox(explicit_flags, &preferences, sandbox_mode);
        let effective_permission_mode =
            resolve_effective_permission_mode(explicit_flags, &preferences, permission_mode);
        let effective_auto_fallback =
            resolve_effective_auto_fallback(explicit_flags, &preferences, auto_fallback);
        let session = match runtime::start_tui_runtime_session(
            active_config.as_ref(),
            session_store.clone(),
            selection,
            &preferences,
            runtime::TuiSessionLaunch {
                sandbox_mode: effective_sandbox,
                permission_mode: effective_permission_mode,
                auto_fallback: effective_auto_fallback,
                configured_permission_mode: permission_mode,
                configured_auto_fallback: auto_fallback,
                explicit_cli_flags: explicit_flags,
                workspace_root: workspace_root.clone(),
                workspace_writable: workspace.writable,
            },
        )
        .await
        {
            Ok(session) => session,
            Err(runtime::TuiSessionStartError::ResumeUnavailable {
                kind,
                session_id,
                session_dir,
            }) => {
                let old_id = session_id.as_str().to_owned();
                let notice = session_picker::SessionPickerNotice {
                    kind,
                    session_dir: session_dir.clone(),
                };
                selection = session_picker::pick_session_with_notice(
                    &mut terminal,
                    &session_store,
                    &workspace_root,
                    &current_picker_theme,
                    Some(notice),
                )
                .await?;
                if matches!(selection, SessionPickerSelection::New) {
                    announce_new_session = true;
                    pending_resume_hint = Some((kind, old_id, session_dir));
                }
                continue;
            }
            Err(runtime::TuiSessionStartError::Fatal(error)) => return Err(error),
        };
        let mut state = TuiState::new(
            session.workspace_root.clone(),
            session.model_label.clone(),
            keymap.clone(),
            configured_theme.clone(),
        )
        .with_successful_command_output(tui_config.show_successful_command_output)
        .with_copy_button_position(tui_config.copy_button_position)
        .with_mouse_scroll_lines(tui_config.mouse_scroll_lines)
        .with_animations(tui_config.animations)
        .with_max_reading_width(tui_config.max_reading_width)
        .with_text_sizing(tui_config.text_sizing)
        .with_brand_mark(tui_config.brand_mark)
        .with_header_items(tui_config.header_items.clone());
        state.set_input_history(input_history_store.load().await);
        state.set_reasoning_effort_label(session.reasoning_effort_label.clone());
        state.set_completion_skills(session.skills.clone());
        state.configure_preferences(preferences.clone(), settings_defaults.clone());
        let plan_snapshot = session.plan_session_snapshot().await;
        let plan_docs = session.plan_documents().await;
        state.plan_mut().hydrate(plan_snapshot, plan_docs);
        let todo_items = session.todo_items().await;
        state.todo_dock_mut().set_items(todo_items);
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
            if let Ok(archived_segments) = session.runtime().archived_segment_numbers().await {
                state.set_unloaded_archive_segments(archived_segments);
            }
            if let Some(subagents) = session.runtime().subagent_snapshot().await {
                state.apply_subagent_status_views(&subagents);
            }
            if let Some(usage) = session.runtime().usage().await {
                state.set_usage(usage);
            }
        } else if announce_new_session {
            state.push_timeline_item(state::TimelineItem::Muted {
                title: "New session".to_owned(),
                detail: session.metadata.session_id.as_str().to_owned(),
            });
        }
        if !session.resumed
            && let Some((kind, old_id, old_dir)) = pending_resume_hint.take()
        {
            let (title, lead) = match kind {
                runtime::ResumeFailureKind::IncompatibleFormat => (
                    "Previous session format incompatible",
                    format!(
                        "Session {old_id} uses a different format and could not be resumed directly."
                    ),
                ),
                runtime::ResumeFailureKind::AlreadyOpen => (
                    "Previous session is open elsewhere",
                    format!("Session {old_id} is currently open in another window."),
                ),
            };
            state.push_timeline_item(state::TimelineItem::LocalCommand {
                title: title.to_owned(),
                body: format!(
                    "{lead}\nYou can ask this new session to read important context from:\n{}",
                    old_dir.display()
                ),
            });
        }
        project_mcp_startup_warnings(&mut state, &session.startup_warnings);
        let exit = controller::run_controller(
            &mut terminal,
            session,
            state,
            &preferences_store,
            &input_history_store,
            &mut provider_management,
        )
        .await?;

        active_config = provider_management
            .config()
            .cloned()
            .or_else(|| merry_config.cloned());
        settings_defaults = TuiSettingsDefaults::from_config(active_config.as_ref())
            .map_err(crate::cli_error::unexpected)?
            .with_explicit_cli_flags(explicit_flags);
        preferences = preferences_store
            .load_with_default_provider(settings_defaults.provider.as_deref())
            .await
            .map_err(crate::cli_error::unexpected)?;
        let picker_transparent = preferences
            .transparent_background
            .unwrap_or(settings_defaults.transparent_background);
        let picker_theme = preferences
            .theme_preset
            .map(TuiTheme::from_preset)
            .unwrap_or_else(|| configured_theme.clone())
            .with_transparent_background(picker_transparent);
        current_picker_theme = picker_theme;

        match exit {
            controller::ControllerExit::Quit => return Ok(()),
            controller::ControllerExit::NewSession => {
                announce_new_session = true;
                selection = SessionPickerSelection::New;
            }
            controller::ControllerExit::ResumeSession => {
                announce_new_session = false;
                selection = session_picker::pick_session(
                    &mut terminal,
                    &session_store,
                    &workspace_root,
                    &current_picker_theme,
                )
                .await?;
            }
        }
    }
}

pub(crate) fn resolve_effective_sandbox(
    explicit_flags: crate::cli::ExplicitCliFlags,
    preferences: &TuiPreferences,
    config_or_default: SandboxMode,
) -> SandboxMode {
    explicit_flags
        .sandbox
        .or(preferences.permissions.sandbox)
        .unwrap_or(config_or_default)
}

pub(crate) fn resolve_effective_permission_mode(
    explicit_flags: crate::cli::ExplicitCliFlags,
    preferences: &TuiPreferences,
    config_or_default: PermissionMode,
) -> PermissionMode {
    explicit_flags
        .permission_mode
        .or(preferences.permissions.permission_mode)
        .unwrap_or(config_or_default)
}

pub(crate) fn resolve_effective_auto_fallback(
    explicit_flags: crate::cli::ExplicitCliFlags,
    preferences: &TuiPreferences,
    config_or_default: AutoFallback,
) -> AutoFallback {
    explicit_flags
        .auto_fallback
        .or(preferences.permissions.auto_fallback)
        .unwrap_or(config_or_default)
}

fn project_mcp_startup_warnings(state: &mut TuiState, warnings: &[merry_mcp::McpServerDiagnostic]) {
    for warning in warnings {
        state.push_timeline_item(state::TimelineItem::Diagnostic {
            title: "MCP startup warning".to_owned(),
            body: crate::mcp_tools::format_mcp_warning(warning),
        });
    }
}
