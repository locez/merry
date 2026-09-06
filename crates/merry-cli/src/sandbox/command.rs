//! Bubblewrap argument construction and execution of the validated bootstrap plan.

use crate::{
    provider_config::MERRY_OPENAI_DEBUG_ENV,
    sandbox::{
        BWRAP_PROGRAM, ChildHandoff, ClipboardAccess, DEFAULT_SANDBOX_PATH, Error,
        MERRY_SANDBOX_ENV, MERRY_SANDBOX_VERSION, MERRY_SANDBOX_VERSION_ENV, Plan,
        SANDBOX_CHILD_HANDOFF_ARG, SANDBOX_ETC_READ_ONLY_DIR_PATHS,
        SANDBOX_ETC_READ_ONLY_FILE_PATHS, SANDBOX_HOME_ROOT, SANDBOX_TMPDIR,
        host::{Host, HostPathProbe},
        integrations::{GraphicalAccessPlan, graphical_access_plan, host_integration_access_plan},
        mounts::{MountOrigin, MountPlan, append_mount_parent_args},
        os,
        paths::SandboxPathPlan,
    },
};
use merry_runtime::PathAccess;
use std::{
    env,
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
};

pub(super) fn build_plan(
    host: &Host,
    path: OsString,
    bwrap: PathBuf,
    clipboard_access: ClipboardAccess,
    probe: &impl HostPathProbe,
    path_plan: &SandboxPathPlan,
) -> Result<Plan, Error> {
    let cwd = host.cwd.as_os_str().to_owned();
    let current_exe = host.current_exe.as_os_str().to_owned();
    let home = host.xdg_paths.home().as_os_str().to_owned();
    let config_base = host.xdg_paths.config_base_dir().as_os_str().to_owned();
    let state_base = host.xdg_paths.state_base_dir().as_os_str().to_owned();
    let config_dir = host.xdg_paths.config_dir().to_path_buf();
    let managed_config_dir = host.xdg_paths.managed_config_dir();
    let state_dir = host.xdg_paths.state_dir().to_path_buf();
    let graphical_plan = match clipboard_access {
        ClipboardAccess::Disabled => GraphicalAccessPlan::default(),
        ClipboardAccess::Tui => graphical_access_plan(host, probe),
    };
    let host_integration_plan = host_integration_access_plan(host, probe);

    let mut args = vec![
        os("--unshare-user"),
        os("--unshare-ipc"),
        os("--unshare-pid"),
        os("--unshare-uts"),
        os("--unshare-cgroup-try"),
        os("--die-with-parent"),
        os("--new-session"),
        os("--proc"),
        os("/proc"),
        os("--dev"),
        os("/dev"),
        os("--perms"),
        os("01777"),
        os("--tmpfs"),
        os(SANDBOX_TMPDIR),
        os("--tmpfs"),
        os(SANDBOX_HOME_ROOT),
    ];
    if !Path::new(&home).starts_with(Path::new(SANDBOX_HOME_ROOT)) {
        append_mount_parent_args(&mut args, Path::new(&home));
        args.extend([os("--tmpfs"), home.clone()]);
    }
    args.extend([os("--perms"), os("0700"), os("--dir"), home.clone()]);
    let mut mounts = MountPlan::default();
    mounts.bind(
        &config_dir,
        &config_dir,
        PathAccess::ReadOnly,
        true,
        MountOrigin::System,
    );
    mounts.bind(
        Path::new("/usr"),
        Path::new("/usr"),
        PathAccess::ReadOnly,
        false,
        MountOrigin::System,
    );
    for path in ["/bin", "/lib", "/lib64", "/opt"] {
        mounts.bind(
            Path::new(path),
            Path::new(path),
            PathAccess::ReadOnly,
            true,
            MountOrigin::System,
        );
    }
    for path in SANDBOX_ETC_READ_ONLY_FILE_PATHS
        .iter()
        .chain(SANDBOX_ETC_READ_ONLY_DIR_PATHS)
    {
        if Path::new(path).exists() {
            mounts.bind(
                Path::new(path),
                Path::new(path),
                PathAccess::ReadOnly,
                false,
                MountOrigin::System,
            );
        }
    }
    mounts.bind(
        &host.cwd,
        &host.cwd,
        PathAccess::ReadWrite,
        false,
        MountOrigin::Workspace,
    );
    if let Some(log_settings) = host.log_settings.as_ref()
        && let Some(host_log_dir) = log_settings.path.parent()
    {
        mounts.bind(
            host_log_dir,
            host_log_dir,
            PathAccess::ReadWrite,
            false,
            MountOrigin::Workspace,
        );
    }
    for rule in &path_plan.development_rules {
        mounts.rule(rule, MountOrigin::Development);
    }
    for rule in &path_plan.trusted_rules {
        mounts.rule(rule, MountOrigin::Trusted);
    }
    mounts.bind(
        &state_dir,
        &state_dir,
        PathAccess::ReadWrite,
        false,
        MountOrigin::Product,
    );
    mounts.bind(
        &managed_config_dir,
        &managed_config_dir,
        PathAccess::ReadWrite,
        false,
        MountOrigin::Product,
    );
    for rule in &path_plan.trusted_product_rules {
        mounts.rule(rule, MountOrigin::ProductRestriction);
    }
    for mount in graphical_plan
        .mounts
        .iter()
        .chain(&host_integration_plan.mounts)
    {
        mounts.bind(
            &mount.source,
            &mount.destination,
            PathAccess::ReadOnly,
            false,
            MountOrigin::Integration,
        );
    }
    mounts.append_args(&mut args).map_err(Error::MountPlan)?;
    args.extend([os("--chdir"), cwd.clone()]);
    args.extend([
        os("--clearenv"),
        os("--setenv"),
        os("PATH"),
        path.clone(),
        os("--setenv"),
        os("HOME"),
        home,
        os("--setenv"),
        os("TMPDIR"),
        os(SANDBOX_TMPDIR),
        os("--setenv"),
        os("XDG_CONFIG_HOME"),
        config_base,
        os("--setenv"),
        os("XDG_STATE_HOME"),
        state_base,
        os("--setenv"),
        os("PWD"),
        cwd,
        os("--setenv"),
        os(MERRY_SANDBOX_ENV),
        os("1"),
        os("--setenv"),
        os(MERRY_SANDBOX_VERSION_ENV),
        os(MERRY_SANDBOX_VERSION),
    ]);
    for (name, value) in graphical_plan.environment {
        args.extend([os("--setenv"), name, value]);
    }
    for (name, value) in host_integration_plan.environment {
        args.extend([os("--setenv"), name, value]);
    }
    for (name, value) in &host.development_environment {
        args.extend([os("--setenv"), name.clone(), value.clone()]);
    }
    if host.openai_debug.as_deref() == Some(OsStr::new("1")) {
        args.extend([os("--setenv"), os(MERRY_OPENAI_DEBUG_ENV), os("1")]);
    }
    args.extend([
        current_exe,
        os(SANDBOX_CHILD_HANDOFF_ARG),
        os(ChildHandoff::CliBwrap.as_cli_value()),
    ]);
    args.extend(args_without_sandbox_bootstrap_flags(&host.args));

    Ok(Plan {
        program: bwrap.as_os_str().to_owned(),
        args,
        env: vec![(os("PATH"), path)],
    })
}

pub(crate) fn find_bwrap_in_path(
    path: &OsStr,
    file_exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    env::split_paths(path)
        .map(|directory| directory.join(BWRAP_PROGRAM))
        .find(|candidate| file_exists(candidate))
}

pub(super) fn sandbox_path(host: &Host) -> OsString {
    host.path
        .as_ref()
        .filter(|value| !value.is_empty())
        .cloned()
        .unwrap_or_else(|| os(DEFAULT_SANDBOX_PATH))
}

pub(crate) fn args_without_sandbox_bootstrap_flags(args: &[OsString]) -> Vec<OsString> {
    let mut removed = false;
    let mut sanitized = Vec::with_capacity(args.len());
    let mut index = 0;
    let mut scanning_root_flags = true;

    while index < args.len() {
        let arg = &args[index];

        if scanning_root_flags {
            if !removed && arg == OsStr::new("--with-sandbox") {
                removed = true;
                index += 1;
                continue;
            }

            if arg == OsStr::new(SANDBOX_CHILD_HANDOFF_ARG) {
                index += 1;
                if index < args.len() {
                    index += 1;
                }
                continue;
            }

            if is_child_handoff_assignment(arg) {
                index += 1;
                continue;
            }

            scanning_root_flags = false;
        }

        sanitized.push(arg.clone());
        index += 1;
    }

    sanitized
}

pub(super) fn is_child_handoff_assignment(arg: &OsStr) -> bool {
    arg.to_str().is_some_and(|value| {
        value
            .strip_prefix(SANDBOX_CHILD_HANDOFF_ARG)
            .is_some_and(|suffix| suffix.starts_with('='))
    })
}

pub(super) fn exec(plan: Plan) -> Result<(), Error> {
    #[cfg(target_os = "linux")]
    {
        let error = exec_plan(&plan);
        if error.kind() == io::ErrorKind::NotFound {
            Err(Error::MissingBubblewrap)
        } else {
            Err(Error::Exec(error))
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = plan;
        Err(Error::UnsupportedPlatform)
    }
}

#[cfg(target_os = "linux")]
pub(super) fn exec_plan(plan: &Plan) -> io::Error {
    use std::os::unix::process::CommandExt;

    let mut command = std::process::Command::new(&plan.program);
    command.args(&plan.args).env_clear().envs(plan.env.clone());
    command.exec()
}
