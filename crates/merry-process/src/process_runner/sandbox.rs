#[cfg(test)]
use super::environment::{ACTION_SANDBOX_HOME_FALLBACK, ACTION_SANDBOX_PATH_FALLBACK};
use super::{
    environment::{ACTION_SANDBOX_TMPDIR, BwrapProcessEnvironment, process_current_dir},
    path_view::ActionPathView,
    permissions::is_git_metadata_path,
};
use crate::resolve_bwrap_path;
use merry_runtime::{
    HostIntegration, PathAccess, PathAccessRule, PathAccessRuleSource, ProcessActionIntent,
    ProcessRunnerError,
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
    #[cfg(target_os = "linux")]
    pub(super) ssh_config: crate::BwrapSshConfigFiles,
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
        gpg_agent_sockets: None,
    };
    bwrap_process_plan_with_environment(
        intent,
        cwd_root,
        &environment,
        network_allowed,
        path_rules,
        bwrap_program,
    )
    .expect("valid test sandbox plan")
}

pub(crate) fn bwrap_process_plan_with_environment(
    intent: &ProcessActionIntent,
    cwd_root: &Path,
    environment: &BwrapProcessEnvironment,
    network_allowed: bool,
    path_rules: &[PathAccessRule],
    bwrap_program: &Path,
) -> Result<BwrapProcessPlan, ProcessRunnerError> {
    let view =
        super::path_view::ActionPathView::prepare(path_rules, &environment.tmp_source, cwd_root)?;
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
    append_bwrap_required_path_rule(&mut args, cwd_root, PathAccess::ReadWrite, &view)?;
    for rule in path_rules {
        if rule.review_required() || rule.access() == PathAccess::Deny {
            continue;
        }
        if rule.source() == PathAccessRuleSource::GitMetadataBaseline {
            append_bwrap_git_metadata_baseline_rule(&mut args, rule.path(), &view)?;
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
            append_bwrap_required_path_rule(&mut args, rule.path(), rule.access(), &view)?;
        } else {
            append_bwrap_path_rule(&mut args, rule.path(), rule.access(), &view)?;
        }
    }
    super::restricted_mounts::append(&mut args, path_rules, &environment.tmp_source, &view)?;
    #[cfg(target_os = "linux")]
    let ssh_config =
        crate::BwrapSshConfigFiles::prepare(Path::new("/etc/ssh/ssh_config"), |path| {
            view.resolve(path)
        })?;
    #[cfg(target_os = "linux")]
    ssh_config.append_args(&mut args);
    if environment
        .host_integrations
        .contains(&HostIntegration::SshAgent)
    {
        for path in crate::ssh_known_hosts(&environment.home) {
            if let Some(mapping) = view.resolve(&path)?
                && mapping.source().is_file()
            {
                args.extend([
                    os("--ro-bind"),
                    mapping.source().as_os_str().to_owned(),
                    mapping.destination().as_os_str().to_owned(),
                ]);
            }
        }
    }
    if let Some(sockets) = environment.gpg_client() {
        super::gpg_client::append(&mut args, sockets, cwd_root, &view)?;
    }
    for (_, socket, _) in environment.host_integration_bindings() {
        if view.visible(&socket) && view.visible(&resolve_bwrap_path(&socket)) {
            append_bwrap_host_integration_mount_args(&mut args, &socket, &view)?;
        }
    }
    let aliases = &view.aliases;
    for path in environment.host_integration_hidden_paths() {
        for (path, source) in aliases.action_paths(&path, &environment.tmp_source) {
            if source.exists() && view.visible(&path) {
                append_bwrap_hidden_host_integration_args(&mut args, &view.destination(&path)?);
            }
        }
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
    if let Some(sockets) = environment.gpg_client() {
        args.extend([
            os("--setenv"),
            os("GNUPGHOME"),
            sockets.home().as_os_str().to_owned(),
        ]);
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

    Ok(BwrapProcessPlan {
        program: bwrap_program.as_os_str().to_owned(),
        args,
        cwd,
        #[cfg(target_os = "linux")]
        ssh_config,
    })
}

fn append_bwrap_hidden_host_integration_args(args: &mut Vec<OsString>, path: &Path) {
    crate::BwrapMaskKind::NonDirectory.append(args, path);
}

fn append_bwrap_host_integration_mount_args(
    args: &mut Vec<OsString>,
    socket: &Path,
    view: &ActionPathView,
) -> Result<(), ProcessRunnerError> {
    args.extend([
        os("--ro-bind"),
        resolve_bwrap_path(socket).into_os_string(),
        view.destination(socket)?.into_os_string(),
    ]);
    Ok(())
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
        HostIntegration::GpgAgent => {
            if let Some(home) = address {
                args.extend([os("--setenv"), os("GNUPGHOME"), home.to_owned()]);
            }
        }
    }
}

fn append_bwrap_path_rule(
    args: &mut Vec<OsString>,
    path: &Path,
    access: PathAccess,
    view: &ActionPathView,
) -> Result<(), ProcessRunnerError> {
    let resolved = resolve_bwrap_path(path);
    let destination = view.destination(&resolved)?;
    let path = destination.as_path();
    match access {
        PathAccess::ReadOnly => args.extend([
            os("--ro-bind-try"),
            resolved.as_os_str().to_owned(),
            path.as_os_str().to_owned(),
        ]),
        PathAccess::ReadWrite => args.extend([
            os("--bind-try"),
            resolved.as_os_str().to_owned(),
            path.as_os_str().to_owned(),
        ]),
        PathAccess::Deny => {
            append_bwrap_mount_parent_args(args, path);
            args.extend([os("--tmpfs"), path.as_os_str().to_owned()]);
        }
    }
    Ok(())
}

fn append_bwrap_required_path_rule(
    args: &mut Vec<OsString>,
    path: &Path,
    access: PathAccess,
    view: &ActionPathView,
) -> Result<(), ProcessRunnerError> {
    let resolved = resolve_bwrap_path(path);
    let destination = view.destination(&resolved)?;
    let path = destination.as_path();
    append_bwrap_mount_parent_args(args, path);
    match access {
        PathAccess::ReadOnly => args.extend([
            os("--ro-bind"),
            resolved.as_os_str().to_owned(),
            path.as_os_str().to_owned(),
        ]),
        PathAccess::ReadWrite => args.extend([
            os("--bind"),
            resolved.as_os_str().to_owned(),
            path.as_os_str().to_owned(),
        ]),
        PathAccess::Deny => args.extend([os("--tmpfs"), path.as_os_str().to_owned()]),
    }
    Ok(())
}

fn append_bwrap_git_metadata_baseline_rule(
    args: &mut Vec<OsString>,
    path: &Path,
    view: &ActionPathView,
) -> Result<(), ProcessRunnerError> {
    if !path.exists() {
        // A missing `.git` is the initialization case. Leaving it under the
        // writable workspace lets `git init` persist metadata; a later plan
        // sees the path and mounts it read-only.
        return Ok(());
    }
    let destination = view.destination(path)?;
    append_bwrap_mount_parent_args(args, &destination);
    args.extend([
        os("--ro-bind"),
        resolve_bwrap_path(path).as_os_str().to_owned(),
        destination.into_os_string(),
    ]);
    Ok(())
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
