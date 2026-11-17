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
        lines.push(format!(
            "(allow file-read* (subpath {escaped}))",
            escaped = quote(path)
        ));
        lines.push(format!(
            "(allow file-write* (subpath {escaped}))",
            escaped = quote(path)
        ));
    }
    for path in &launch.traverse {
        lines.push(format!(
            "(allow file-read-metadata (subpath {escaped}))",
            escaped = quote(path)
        ));
    }
    lines.join("\n")
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
    command
        .arg("-p")
        .arg(profile(launch))
        .arg("--")
        .arg(&launch.program)
        .args(&launch.args);
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
