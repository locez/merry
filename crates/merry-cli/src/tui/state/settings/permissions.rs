//! Permissions configuration helpers for the Settings Workbench.

use super::TuiState;
use crate::{
    coding::{AutoFallback, PermissionMode, SandboxMode},
    tui::overlay::{SettingDirection, SettingItem},
};

pub(super) const SANDBOX_MODES: [Option<SandboxMode>; 3] =
    [None, Some(SandboxMode::Workspace), Some(SandboxMode::Off)];
pub(super) const PERMISSION_MODES: [Option<PermissionMode>; 5] = [
    None,
    Some(PermissionMode::Auto),
    Some(PermissionMode::Ask),
    Some(PermissionMode::AlwaysApprove),
    Some(PermissionMode::Deny),
];
pub(super) const AUTO_FALLBACKS: [Option<AutoFallback>; 3] =
    [None, Some(AutoFallback::Ask), Some(AutoFallback::Deny)];

impl TuiState {
    pub(super) fn adjust_permission_setting(
        &mut self,
        item: SettingItem,
        direction: SettingDirection,
    ) -> bool {
        let perms = &mut self.preferences.permissions;
        match item {
            SettingItem::PermissionSandbox => {
                super::cycle_slice(&mut perms.sandbox, &SANDBOX_MODES, direction);
                true
            }
            SettingItem::PermissionMode => {
                super::cycle_slice(&mut perms.permission_mode, &PERMISSION_MODES, direction);
                true
            }
            SettingItem::PermissionAutoFallback => {
                super::cycle_slice(&mut perms.auto_fallback, &AUTO_FALLBACKS, direction);
                true
            }
            SettingItem::PermissionNetwork => {
                super::cycle_opt_bool(&mut perms.network, direction);
                true
            }
            SettingItem::PermissionSshAgent => {
                super::cycle_opt_bool(&mut perms.ssh_agent, direction);
                true
            }
            SettingItem::PermissionGpgAgent => {
                super::cycle_opt_bool(&mut perms.gpg_agent, direction);
                true
            }
            SettingItem::PermissionDbus => {
                super::cycle_opt_bool(&mut perms.dbus, direction);
                true
            }
            _ => false,
        }
    }

    pub(super) fn reset_permission_setting(&mut self, item: SettingItem) -> bool {
        let perms = &mut self.preferences.permissions;
        match item {
            SettingItem::PermissionSandbox => perms.sandbox = None,
            SettingItem::PermissionMode => perms.permission_mode = None,
            SettingItem::PermissionAutoFallback => perms.auto_fallback = None,
            SettingItem::PermissionNetwork => perms.network = None,
            SettingItem::PermissionSshAgent => perms.ssh_agent = None,
            SettingItem::PermissionGpgAgent => perms.gpg_agent = None,
            SettingItem::PermissionDbus => perms.dbus = None,
            _ => return false,
        }
        true
    }

    pub(super) fn is_permission_setting_overridden(&self, item: SettingItem) -> bool {
        let perms = &self.preferences.permissions;
        match item {
            SettingItem::PermissionSandbox => perms.sandbox.is_some(),
            SettingItem::PermissionMode => perms.permission_mode.is_some(),
            SettingItem::PermissionAutoFallback => perms.auto_fallback.is_some(),
            SettingItem::PermissionNetwork => perms.network.is_some(),
            SettingItem::PermissionSshAgent => perms.ssh_agent.is_some(),
            SettingItem::PermissionGpgAgent => perms.gpg_agent.is_some(),
            SettingItem::PermissionDbus => perms.dbus.is_some(),
            _ => false,
        }
    }

    pub(super) fn permission_setting_value(&self, item: SettingItem) -> String {
        let perms = &self.preferences.permissions;
        let flags = self.settings_defaults.explicit_cli_flags;
        match item {
            SettingItem::PermissionSandbox => {
                if let Some(flag) = flags.sandbox {
                    format!("{} [CLI flag]", sandbox_label(flag))
                } else {
                    perms
                        .sandbox
                        .map(sandbox_label)
                        .map(str::to_owned)
                        .unwrap_or_else(|| {
                            format!(
                                "Inherit ({})",
                                sandbox_label(self.settings_defaults.sandbox)
                            )
                        })
                }
            }
            SettingItem::PermissionMode => {
                if let Some(flag) = flags.permission_mode {
                    format!("{} [CLI flag]", permission_mode_label(flag))
                } else {
                    perms
                        .permission_mode
                        .map(permission_mode_label)
                        .map(str::to_owned)
                        .unwrap_or_else(|| {
                            format!(
                                "Inherit ({})",
                                permission_mode_label(self.settings_defaults.permission_mode)
                            )
                        })
                }
            }
            SettingItem::PermissionAutoFallback => {
                if let Some(flag) = flags.auto_fallback {
                    format!("{} [CLI flag]", auto_fallback_label(flag))
                } else {
                    perms
                        .auto_fallback
                        .map(auto_fallback_label)
                        .map(str::to_owned)
                        .unwrap_or_else(|| {
                            format!(
                                "Inherit ({})",
                                auto_fallback_label(self.settings_defaults.auto_fallback)
                            )
                        })
                }
            }
            SettingItem::PermissionNetwork => {
                super::opt_bool_value(perms.network, self.settings_defaults.network_allowed)
            }
            SettingItem::PermissionSshAgent => {
                super::opt_bool_value(perms.ssh_agent, self.settings_defaults.ssh_agent)
            }
            SettingItem::PermissionGpgAgent => {
                super::opt_bool_value(perms.gpg_agent, self.settings_defaults.gpg_agent)
            }
            SettingItem::PermissionDbus => {
                super::opt_bool_value(perms.dbus, self.settings_defaults.dbus)
            }
            _ => String::new(),
        }
    }
}

pub(super) fn sandbox_label(mode: SandboxMode) -> &'static str {
    match mode {
        SandboxMode::Workspace => "workspace",
        SandboxMode::Off => "off (host)",
    }
}

pub(super) fn permission_mode_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Auto => "auto",
        PermissionMode::Ask => "ask",
        PermissionMode::AlwaysApprove => "always-approve",
        PermissionMode::Deny => "deny",
    }
}

pub(super) fn auto_fallback_label(mode: AutoFallback) -> &'static str {
    match mode {
        AutoFallback::Ask => "ask",
        AutoFallback::Deny => "deny",
    }
}
