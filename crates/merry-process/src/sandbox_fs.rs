//! Shared filesystem mechanics; callers retain admission and mount precedence policy.

use merry_runtime::ProcessRunnerError;
use std::{ffi::OsString, fs, path::Path};

/// The object shape required by a bubblewrap protection mount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BwrapMaskKind {
    /// An empty private directory, sealed after descendant mounts are prepared.
    Directory,
    /// An unreadable-content replacement for a file or Unix socket.
    NonDirectory,
}

impl BwrapMaskKind {
    /// Inspects a configured protection source without creating host paths.
    /// Missing targets fail closed: neither their future type nor a safe mount
    /// point inside an imported read-only parent is available yet.
    pub fn inspect(path: &Path) -> Result<Self, ProcessRunnerError> {
        fs::metadata(path)
            .map(|metadata| if metadata.is_dir() { Self::Directory } else { Self::NonDirectory })
            .map_err(|error| ProcessRunnerError::infrastructure(format!(
                "cannot protect sandbox path `{}`: {error}; create the configured target first or protect an existing ancestor; no host path was created",
                path.display()
            )))
    }

    /// Appends an empty protection mount. Directory callers must subsequently
    /// append `--remount-ro` unless an admitted grant replaces that exact mount.
    pub fn append(self, args: &mut Vec<OsString>, destination: &Path) {
        match self {
            Self::Directory => args.extend([
                OsString::from("--tmpfs"),
                destination.as_os_str().to_owned(),
            ]),
            Self::NonDirectory => args.extend([
                OsString::from("--ro-bind"),
                OsString::from("/dev/null"),
                destination.as_os_str().to_owned(),
            ]),
        }
    }
}
