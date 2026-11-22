//! Seatbelt profile applied by `/usr/bin/sandbox-exec` on macOS.
//!
//! The profile is pure text, so Linux tests can check it. This host cannot
//! execute Seatbelt.

#[cfg(any(test, target_os = "macos"))]
use super::SandboxLaunch;

#[cfg(any(test, target_os = "macos"))]
pub(crate) fn profile(launch: &SandboxLaunch) -> String {
    let mut lines = vec![
        "(version 1)".to_owned(),
        "(deny default)".to_owned(),
        "(allow process-exec)".to_owned(),
        "(allow process-fork)".to_owned(),
        "(allow signal (target self))".to_owned(),
        "(allow sysctl-read)".to_owned(),
        "(allow mach-lookup)".to_owned(),
        "(allow ipc-posix*)".to_owned(),
        "(allow system-socket)".to_owned(),
        "(allow file-ioctl)".to_owned(),
        "(allow file-read-metadata)".to_owned(),
        "(allow file-read-data (literal \"/\"))".to_owned(),
    ];
    if launch.network() {
        lines.push("(allow network*)".to_owned());
    } else {
        lines.push("(deny network*)".to_owned());
    }
    for path in &launch.read_only {
        lines.push(format!(
            "(allow file-read* (subpath {escaped}))",
            escaped = quote(path)
        ));
    }
    for path in &launch.read_write {
        let escaped = quote(path);
        lines.push(format!("(allow file-read* (subpath {escaped}))"));
        lines.push(format!("(allow file-write* (subpath {escaped}))"));
        if !launch.network() {
            // Keep AF_UNIX available inside granted writable paths and enabled
            // host-integration sockets (matching Linux seccomp) while IP
            // sockets remain blocked by `(deny network*)`.
            lines.push(format!(
                "(allow network-bind (local unix-socket (subpath {escaped})))"
            ));
            lines.push(format!(
                "(allow network-inbound (local unix-socket (subpath {escaped})))"
            ));
            lines.push(format!(
                "(allow network-outbound (remote unix-socket (subpath {escaped})))"
            ));
        }
    }
    for path in &launch.traverse {
        lines.push(format!(
            "(allow file-read-metadata (subpath {escaped}))",
            escaped = quote(path)
        ));
    }
    // Seatbelt evaluates matching rules last-wins, so emitting explicit deny
    // rules after allows still protects private subtrees when a user grants a
    // broader parent directory (such as `~/.local/share` or `~`).
    for path in &launch.deny {
        let escaped = quote(path);
        lines.push(format!("(deny file-read* (subpath {escaped}))"));
        lines.push(format!("(deny file-write* (subpath {escaped}))"));
    }
    lines.join("\n")
}

#[cfg(any(test, target_os = "macos"))]
pub(crate) fn command_args(launch: &SandboxLaunch) -> Vec<String> {
    let mut args = Vec::with_capacity(launch.args.len() + 4);
    args.push("-p".to_owned());
    args.push(profile(launch));
    args.push("--".to_owned());
    args.push(launch.program.clone());
    args.extend(launch.args.iter().cloned());
    args
}

#[cfg(target_os = "macos")]
pub(crate) fn build_tokio_command(launch: &SandboxLaunch) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("/usr/bin/sandbox-exec");
    command
        .args(command_args(launch))
        .current_dir(&launch.cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for (name, value) in &launch.env {
        command.env(name, value);
    }
    for name in &launch.remove_env {
        command.env_remove(name);
    }
    command
        .env_remove(super::HELPER_ENV)
        .env_remove(super::POLICY_ENV);
    command
}

#[cfg(any(test, target_os = "macos"))]
fn quote(path: &str) -> String {
    format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(target_os = "macos")]
pub(super) fn probe() -> Result<(), String> {
    if std::path::Path::new("/usr/bin/sandbox-exec").is_file() {
        Ok(())
    } else {
        Err(
            "workspace sandbox needs /usr/bin/sandbox-exec. Pass --sandbox off to run without it."
                .to_owned(),
        )
    }
}

/// Replaces this process with `sandbox-exec -p`.
///
/// The profile is an argument, so a successful exec does not leave a profile
/// file behind. This returns only when `sandbox-exec` itself cannot start.
#[cfg(target_os = "macos")]
pub(super) fn apply(launch: &SandboxLaunch) -> Result<(), String> {
    use super::CommandExec;
    use std::process::Command;

    let mut command = Command::new("/usr/bin/sandbox-exec");
    command.args(command_args(launch));
    command.current_dir(&launch.cwd);
    for (name, value) in &launch.env {
        command.env(name, value);
    }
    for name in &launch.remove_env {
        command.env_remove(name);
    }
    let error = command.exec_error();
    Err(format!("sandbox-exec failed: {error}"))
}
