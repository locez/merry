use super::path_view::ActionPathView;
use crate::{GpgAgentSockets, resolve_bwrap_path};
use merry_runtime::ProcessRunnerError;
use std::{ffi::OsString, fs, io, path::Path};

/// Imports public keys while keeping all client writes local to this action.
pub(super) fn append(
    args: &mut Vec<OsString>,
    sockets: &GpgAgentSockets,
    workspace: &Path,
    view: &ActionPathView,
) -> Result<(), ProcessRunnerError> {
    sockets.validate_public_key_access()?;
    let home = resolve_bwrap_path(sockets.home());
    let mut public_keyrings = Vec::new();
    for path in sockets.public_keyrings() {
        let source = resolve_bwrap_path(&path);
        if !view.visible(&source) || !view.visible(&path) {
            continue;
        }
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => public_keyrings.push((source, path)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(ProcessRunnerError::infrastructure(
                    "GPG public-key store must be a regular file",
                ));
            }
            Err(error) => {
                return Err(ProcessRunnerError::infrastructure(format!(
                    "cannot inspect GPG public-key store: {error}"
                )));
            }
        }
    }
    if public_keyrings.is_empty() && !view.visible(&home) {
        return Ok(());
    }
    if resolve_bwrap_path(workspace).starts_with(&home) {
        return Err(ProcessRunnerError::infrastructure(
            "GPG home must not contain the process workspace",
        ));
    }
    match fs::metadata(&home) {
        Ok(metadata) if metadata.is_dir() => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Ok(_) => {
            return Err(ProcessRunnerError::infrastructure(
                "GPG home must be a directory",
            ));
        }
        Err(error) => {
            return Err(ProcessRunnerError::infrastructure(format!(
                "cannot inspect GPG home: {error}"
            )));
        }
    }
    args.extend([
        OsString::from("--perms"),
        OsString::from("0700"),
        OsString::from("--tmpfs"),
        home.as_os_str().to_owned(),
    ]);
    for (source, path) in public_keyrings {
        args.extend([
            OsString::from("--ro-bind"),
            source.into_os_string(),
            home.join(path.file_name().ok_or_else(|| {
                ProcessRunnerError::infrastructure("invalid GPG public-key store path")
            })?)
            .into_os_string(),
        ]);
    }
    Ok(())
}
