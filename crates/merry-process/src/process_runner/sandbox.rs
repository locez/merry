#[cfg(test)]
use super::environment::{ACTION_SANDBOX_HOME_FALLBACK, ACTION_SANDBOX_PATH_FALLBACK};
use super::{
    environment::{ACTION_SANDBOX_TMPDIR, BwrapProcessEnvironment, process_current_dir},
    permissions::is_git_metadata_path,
};
use crate::resolve_bwrap_path;
use merry_runtime::{
    HostIntegration, PathAccess, PathAccessRule, PathAccessRuleSource, ProcessActionIntent,
};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

fn os(value: &str) -> OsString {
    OsString::from(value)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BwrapProcessPlan {
    pub(super) program: OsString,
    pub(super) args: Vec<OsString>,
    pub(super) cwd: PathBuf,
}

#[cfg(test)]
pub(crate) fn bwrap_process_plan(
    intent: &ProcessActionIntent,
    cwd_root: &Path,
    network_allowed: bool,
    path_rules: &[PathAccessRule],
    bwrap_program: &Path,
) -> BwrapProcessPlan {
    let environment = BwrapProcessEnvironment {
        path: OsString::from(ACTION_SANDBOX_PATH_FALLBACK),
        home: PathBuf::from(ACTION_SANDBOX_HOME_FALLBACK),
        tmp_source: PathBuf::from(ACTION_SANDBOX_TMPDIR),
        overrides: Vec::new(),
        host_integrations: Vec::new(),
        ssh_agent_socket: None,
        session_bus_address: None,
    };
    bwrap_process_plan_with_environment(
        intent,
        cwd_root,
        &environment,
        network_allowed,
        path_rules,
        bwrap_program,
    )
}

pub(crate) fn bwrap_process_plan_with_environment(
    intent: &ProcessActionIntent,
    cwd_root: &Path,
    environment: &BwrapProcessEnvironment,
    network_allowed: bool,
    path_rules: &[PathAccessRule],
    bwrap_program: &Path,
) -> BwrapProcessPlan {
    let cwd = process_current_dir(Some(cwd_root), intent);
    let mut args = vec![
        os("--unshare-user"),
        os("--unshare-ipc"),
        os("--unshare-pid"),
        os("--unshare-uts"),
        os("--unshare-cgroup-try"),
        os("--die-with-parent"),
        os("--new-session"),
        os("--ro-bind"),
        os("/"),
        os("/"),
        os("--proc"),
        os("/proc"),
        os("--dev"),
        os("/dev"),
        os("--tmpfs"),
        os(ACTION_SANDBOX_TMPDIR),
        os("--bind"),
        resolve_bwrap_path(&environment.tmp_source)
            .as_os_str()
            .to_owned(),
        os(ACTION_SANDBOX_TMPDIR),
    ];
    if !environment.home.exists() {
        append_bwrap_mount_parent_args(&mut args, &environment.home);
        args.extend([
            os("--tmpfs"),
            environment.home.as_os_str().to_owned(),
            os("--perms"),
            os("0700"),
            os("--dir"),
            environment.home.as_os_str().to_owned(),
        ]);
    }
    if !network_allowed {
        args.push(os("--unshare-net"));
    }
    for path in environment.host_integration_hidden_paths() {
        append_bwrap_hidden_host_integration_args(&mut args, &path);
    }
    append_bwrap_required_path_rule(&mut args, cwd_root, PathAccess::ReadWrite);
    for rule in path_rules {
        if rule.source() == PathAccessRuleSource::GitMetadataBaseline {
            append_bwrap_git_metadata_baseline_rule(&mut args, rule.path());
        } else if rule.source() == PathAccessRuleSource::PermissionReview
            && rule.access() == PathAccess::ReadWrite
            && is_git_metadata_path(rule.path())
            && !rule.path().exists()
        {
            // The workspace or an already approved parent supplies the writable
            // mount for a newly-created .git directory. A required bind would
            // fail before git init gets a chance to create the directory.
            continue;
        } else if rule.source() == PathAccessRuleSource::PermissionReview {
            append_bwrap_required_path_rule(&mut args, rule.path(), rule.access());
        } else {
            append_bwrap_path_rule(&mut args, rule.path(), rule.access());
        }
    }
    for (_, socket, _) in environment.host_integration_bindings() {
        append_bwrap_host_integration_mount_args(&mut args, &socket);
    }
    args.extend([
        os("--chdir"),
        cwd.as_os_str().to_owned(),
        os("--setenv"),
        os("PATH"),
        environment.path.clone(),
        os("--setenv"),
        os("HOME"),
        environment.home.as_os_str().to_owned(),
        os("--setenv"),
        os("TMPDIR"),
        os(ACTION_SANDBOX_TMPDIR),
        os("--setenv"),
        os("PWD"),
        cwd.as_os_str().to_owned(),
    ]);
    for (name, value) in &environment.overrides {
        args.extend([os("--setenv"), name.clone(), value.clone()]);
    }
    args.extend([
        os("--unsetenv"),
        os("SSH_AUTH_SOCK"),
        os("--unsetenv"),
        os("DBUS_SESSION_BUS_ADDRESS"),
    ]);
    for (integration, socket, address) in environment.host_integration_bindings() {
        append_bwrap_host_integration_environment_args(
            &mut args,
            integration,
            &socket,
            address.as_deref(),
        );
    }
    args.push(os("--"));
    args.extend(intent.argv().iter().map(OsString::from));

    BwrapProcessPlan {
        program: bwrap_program.as_os_str().to_owned(),
        args,
        cwd,
    }
}

fn append_bwrap_file_bind_args(args: &mut Vec<OsString>, source: &Path, destination: &Path) {
    append_bwrap_mount_parent_args(args, destination);
    args.extend([
        os("--ro-bind"),
        resolve_bwrap_path(source).as_os_str().to_owned(),
        destination.as_os_str().to_owned(),
    ]);
}

fn append_bwrap_hidden_host_integration_args(args: &mut Vec<OsString>, path: &Path) {
    append_bwrap_mount_parent_args(args, path);
    args.extend([os("--tmpfs"), path.as_os_str().to_owned()]);
}

fn append_bwrap_host_integration_mount_args(args: &mut Vec<OsString>, socket: &Path) {
    append_bwrap_file_bind_args(args, socket, socket);
}

fn append_bwrap_host_integration_environment_args(
    args: &mut Vec<OsString>,
    integration: HostIntegration,
    socket: &Path,
    address: Option<&OsStr>,
) {
    match integration {
        HostIntegration::SshAgent => args.extend([
            os("--setenv"),
            os("SSH_AUTH_SOCK"),
            socket.as_os_str().to_owned(),
        ]),
        HostIntegration::SessionBus => {
            if let Some(address) = address {
                args.extend([
                    os("--setenv"),
                    os("DBUS_SESSION_BUS_ADDRESS"),
                    address.to_owned(),
                ]);
            }
        }
    }
}

fn append_bwrap_path_rule(args: &mut Vec<OsString>, path: &Path, access: PathAccess) {
    append_bwrap_mount_parent_args(args, path);
    match access {
        PathAccess::ReadOnly => args.extend([
            os("--ro-bind-try"),
            resolve_bwrap_path(path).as_os_str().to_owned(),
            path.as_os_str().to_owned(),
        ]),
        PathAccess::ReadWrite => args.extend([
            os("--bind-try"),
            resolve_bwrap_path(path).as_os_str().to_owned(),
            path.as_os_str().to_owned(),
        ]),
        PathAccess::Deny => {
            args.extend([os("--tmpfs"), path.as_os_str().to_owned()]);
        }
    }
}

fn append_bwrap_required_path_rule(args: &mut Vec<OsString>, path: &Path, access: PathAccess) {
    append_bwrap_mount_parent_args(args, path);
    match access {
        PathAccess::ReadOnly => args.extend([
            os("--ro-bind"),
            resolve_bwrap_path(path).as_os_str().to_owned(),
            path.as_os_str().to_owned(),
        ]),
        PathAccess::ReadWrite => args.extend([
            os("--bind"),
            resolve_bwrap_path(path).as_os_str().to_owned(),
            path.as_os_str().to_owned(),
        ]),
        PathAccess::Deny => args.extend([os("--tmpfs"), path.as_os_str().to_owned()]),
    }
}

fn append_bwrap_git_metadata_baseline_rule(args: &mut Vec<OsString>, path: &Path) {
    if !path.exists() {
        // A missing `.git` is the initialization case. Leaving it under the
        // writable workspace lets `git init` persist metadata; a later plan
        // sees the path and mounts it read-only.
        return;
    }
    append_bwrap_mount_parent_args(args, path);
    args.extend([
        os("--ro-bind"),
        resolve_bwrap_path(path).as_os_str().to_owned(),
        path.as_os_str().to_owned(),
    ]);
}

fn append_bwrap_mount_parent_args(args: &mut Vec<OsString>, destination: &Path) {
    let Some(parent) = destination.parent() else {
        return;
    };
    let mut parents = parent
        .ancestors()
        .take_while(|path| *path != Path::new("/"))
        .collect::<Vec<_>>();
    parents.reverse();

    for parent in parents {
        args.extend([os("--dir"), parent.as_os_str().to_owned()]);
    }
}
