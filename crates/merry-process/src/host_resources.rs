//! Host-client resource declarations and filesystem metadata shared by both sandboxes.

use std::path::{Path, PathBuf};

/// A host filesystem object's relevant client-integration shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostPathKind {
    /// A directory.
    Directory,
    /// A regular file, suitable for public client data.
    RegularFile,
    /// A Unix-domain socket, not a file that merely has a socket-like name.
    UnixSocket,
    /// An object not eligible for ordinary client resources.
    Other,
}

/// Inspected metadata or equivalent evidence supplied by a host probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostPathMetadata {
    kind: HostPathKind,
    owner_uid: u32,
    mode: u32,
}

impl HostPathMetadata {
    /// Constructs typed host evidence, without granting any resource access.
    #[must_use]
    pub const fn new(kind: HostPathKind, owner_uid: u32, mode: u32) -> Self {
        Self {
            kind,
            owner_uid,
            mode,
        }
    }

    /// Reads metadata from the source namespace, following symlinks like bind mounts.
    #[cfg(unix)]
    pub fn inspect(path: &Path) -> std::io::Result<Self> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let metadata = std::fs::metadata(path)?;
        let file_type = metadata.file_type();
        let kind = if file_type.is_socket() {
            HostPathKind::UnixSocket
        } else if file_type.is_file() {
            HostPathKind::RegularFile
        } else if file_type.is_dir() {
            HostPathKind::Directory
        } else {
            HostPathKind::Other
        };
        Ok(Self::new(kind, metadata.uid(), metadata.mode()))
    }

    /// Returns the inspected object shape.
    #[must_use]
    pub const fn kind(self) -> HostPathKind {
        self.kind
    }

    /// Returns the source owner's user ID.
    #[must_use]
    pub const fn owner_uid(self) -> u32 {
        self.owner_uid
    }

    /// Returns the source mode bits.
    #[must_use]
    pub const fn mode(self) -> u32 {
        self.mode
    }

    /// Checks the shared native-agent endpoint ownership requirement.
    #[must_use]
    pub fn is_owned_socket(self, current_uid: u32) -> bool {
        self.kind == HostPathKind::UnixSocket && self.owner_uid == current_uid
    }
}

/// Conventional public SSH host databases supplied by the SSH-agent integration.
/// This never includes SSH configuration or private identity files.
#[must_use]
pub fn ssh_known_hosts(home: &Path) -> [PathBuf; 2] {
    [
        home.join(".ssh/known_hosts"),
        home.join(".ssh/known_hosts2"),
    ]
}
