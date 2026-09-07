//! Read-only SSH configuration snapshots for user-namespace ownership compatibility.

mod includes;

use command_fds::{CommandFdExt, FdMapping};
use merry_runtime::ProcessRunnerError;
use rustix::fs::{MemfdFlags, Mode, OFlags, SealFlags};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fmt,
    fs::File,
    io::{Read, Seek, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

const MAX_FILES: usize = 256;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, PartialEq, Eq)]
struct Snapshot {
    destination: PathBuf,
    contents: Arc<[u8]>,
}

impl fmt::Debug for Snapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Snapshot")
            .field("destination", &self.destination)
            .field("bytes", &self.contents.len())
            .finish_non_exhaustive()
    }
}

/// Private, bounded copies of SSH system configuration that lose root ownership
/// when entering a user namespace. This does not grant filesystem access.
///
/// Paths are admitted by the caller's existing mount policy. Unsafe ownership or
/// write permissions are never normalized, and configuration contents are never logged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BwrapSshConfigFiles {
    snapshots: Vec<Snapshot>,
    compatibility_failure: Option<ProcessRunnerError>,
}

#[derive(Debug, thiserror::Error)]
enum PreparationError {
    #[error("SSH namespace policy resolution failed: {0}")]
    Policy(#[source] ProcessRunnerError),
    #[error(transparent)]
    Compatibility(#[from] ProcessRunnerError),
}

impl BwrapSshConfigFiles {
    /// Reads an already-exposed system configuration and its static `Include` paths.
    ///
    /// `source_for` translates a destination in the planned sandbox into its allowed
    /// host source, or returns `None` when absent, denied, or awaiting review. It is
    /// consulted for directories as well as files; this method never grants access.
    /// Includes are not executed and do not alter OpenSSH's configuration text.
    ///
    /// Missing, unreadable, or insecure files retain their original representation.
    /// Discovery failures leave the original configuration unchanged and emit a
    /// warning, available through `compatibility_failure`. Policy resolution
    /// failures remain fatal. OpenSSH still performs its own security checks.
    pub fn prepare(
        root: &Path,
        source_for: impl Fn(&Path) -> Result<Option<PathBuf>, ProcessRunnerError>,
    ) -> Result<Self, ProcessRunnerError> {
        match Self::discover(root, source_for) {
            Ok(files) => Ok(files),
            Err(PreparationError::Policy(error)) => Err(error),
            Err(PreparationError::Compatibility(error)) => {
                tracing::warn!(%error, "SSH ownership compatibility unavailable; original configuration and OpenSSH security checks remain in effect");
                Ok(Self {
                    snapshots: Vec::new(),
                    compatibility_failure: Some(error),
                })
            }
        }
    }

    /// Reports why ownership adaptation was unavailable, without configuration contents.
    #[must_use]
    pub fn compatibility_failure(&self) -> Option<&ProcessRunnerError> {
        self.compatibility_failure.as_ref()
    }

    fn discover(
        root: &Path,
        source_for: impl Fn(&Path) -> Result<Option<PathBuf>, ProcessRunnerError>,
    ) -> Result<Self, PreparationError> {
        let base = root
            .parent()
            .ok_or_else(|| failure("invalid system SSH config path"))?;
        let mut pending = vec![root.to_path_buf()];
        let mut visited = BTreeSet::new();
        let mut snapshots = Vec::new();
        let mut total_bytes = 0;
        let current_uid = rustix::process::getuid().as_raw();
        while let Some(destination) = pending.pop() {
            let Some(source) = source_for(&destination).map_err(PreparationError::Policy)? else {
                continue;
            };
            if !visited.insert((destination.clone(), source.clone())) {
                continue;
            }
            if visited.len() > MAX_FILES {
                return Err(failure("system SSH configuration exceeds 256 files").into());
            }
            let descriptor = match rustix::fs::open(
                &source,
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::empty(),
            ) {
                Ok(descriptor) => descriptor,
                Err(
                    rustix::io::Errno::NOENT
                    | rustix::io::Errno::ACCESS
                    | rustix::io::Errno::NOTDIR,
                ) => continue,
                Err(error) => return Err(io_failure("open system SSH configuration", error).into()),
            };
            let file = File::from(descriptor);
            let metadata = file
                .metadata()
                .map_err(|error| io_failure("inspect system SSH configuration", error))?;
            if !metadata.is_file()
                || (metadata.uid() != 0 && metadata.uid() != current_uid)
                || metadata.mode() & 0o022 != 0
            {
                continue;
            }
            let mut contents = Vec::new();
            file.take(MAX_FILE_BYTES + 1)
                .read_to_end(&mut contents)
                .map_err(|error| io_failure("read system SSH configuration", error))?;
            if contents.len() > MAX_FILE_BYTES as usize {
                return Err(failure("system SSH configuration file exceeds 1 MiB").into());
            }
            total_bytes += contents.len();
            if total_bytes > MAX_TOTAL_BYTES {
                return Err(failure("system SSH configuration exceeds 4 MiB").into());
            }
            if let Ok(text) = std::str::from_utf8(&contents) {
                for pattern in includes::paths(text, base) {
                    pending.extend(includes::expand(&pattern, &source_for)?);
                    if pending.len() > MAX_FILES {
                        return Err(
                            failure("system SSH Include expansion exceeds 256 files").into()
                        );
                    }
                }
            }
            if metadata.uid() == 0 && current_uid != 0 {
                snapshots.push(Snapshot {
                    destination,
                    contents: contents.into(),
                });
            }
        }
        snapshots.sort_by(|left, right| left.destination.cmp(&right.destination));
        Ok(Self {
            snapshots,
            compatibility_failure: None,
        })
    }

    /// Appends read-only data mounts after ordinary mounts. The caller must also
    /// call `configure_command` on the same bubblewrap invocation before spawning it.
    pub fn append_args(&self, args: &mut Vec<OsString>) {
        for (index, snapshot) in self.snapshots.iter().enumerate() {
            args.extend([
                OsString::from("--perms"),
                OsString::from("0400"),
                OsString::from("--ro-bind-data"),
                OsString::from((index + 3).to_string()),
                snapshot.destination.as_os_str().to_owned(),
            ]);
        }
    }

    /// Supplies sealed anonymous-memory snapshots on child-only descriptors.
    ///
    /// Each invocation gets independent file offsets. The command owns its FDs,
    /// including on spawn/exec failure; bubblewrap consumes and closes the child
    /// copies before executing the action. No caller FD has CLOEXEC cleared.
    pub fn configure_command(&self, command: &mut Command) -> Result<(), ProcessRunnerError> {
        let mut mappings = Vec::with_capacity(self.snapshots.len());
        for (index, snapshot) in self.snapshots.iter().enumerate() {
            let descriptor = rustix::fs::memfd_create(
                "merry-ssh-config",
                MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
            )
            .map_err(|error| io_failure("create SSH configuration snapshot", error))?;
            let mut file = File::from(descriptor);
            file.write_all(&snapshot.contents)
                .and_then(|()| file.rewind())
                .map_err(|error| io_failure("prepare SSH configuration snapshot", error))?;
            rustix::fs::fcntl_add_seals(
                &file,
                SealFlags::WRITE | SealFlags::GROW | SealFlags::SHRINK | SealFlags::SEAL,
            )
            .map_err(|error| io_failure("seal SSH configuration snapshot", error))?;
            let child_fd = i32::try_from(index + 3)
                .map_err(|error| io_failure("assign SSH configuration descriptor", error))?;
            mappings.push(FdMapping {
                parent_fd: file.into(),
                child_fd,
            });
        }
        if !mappings.is_empty() {
            command
                .fd_mappings(mappings)
                .map_err(|error| io_failure("map SSH configuration descriptors", error))?;
        }
        Ok(())
    }
}

fn failure(message: &str) -> ProcessRunnerError {
    ProcessRunnerError::infrastructure(message)
}

fn io_failure(operation: &str, error: impl fmt::Display) -> ProcessRunnerError {
    ProcessRunnerError::infrastructure(format!("cannot {operation}: {error}"))
}

#[cfg(test)]
mod tests;
