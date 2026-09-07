use crate::resolve_bwrap_path;
use merry_runtime::ProcessRunnerError;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Default)]
pub(super) struct MountAliases {
    mounts: Vec<Mount>,
}

struct Mount {
    device: String,
    root: PathBuf,
    destination: PathBuf,
}

impl MountAliases {
    /// Grants only aliases of the same object, never a separately mounted descendant.
    pub(super) fn grant_paths(&self, path: &Path, tmp_source: &Path) -> BTreeMap<PathBuf, PathBuf> {
        self.action_paths(path, tmp_source)
            .into_iter()
            .filter(|(_, source)| same_file(source, path))
            .collect()
    }

    /// Includes the extra alias introduced when an action maps TMPDIR onto /tmp.
    pub(super) fn action_paths(
        &self,
        path: &Path,
        tmp_source: &Path,
    ) -> BTreeMap<PathBuf, PathBuf> {
        let paths = self.paths(path);
        let mut mappings = paths
            .iter()
            .map(|path| (path.clone(), path.clone()))
            .collect::<BTreeMap<_, _>>();
        let tmp_source = resolve_bwrap_path(tmp_source);
        if tmp_source != Path::new("/tmp") {
            for source in paths {
                if let Ok(relative) = source.strip_prefix(&tmp_source) {
                    mappings.insert(Path::new("/tmp").join(relative), source);
                } else if tmp_source.starts_with(&source) {
                    mappings.insert(PathBuf::from("/tmp"), tmp_source.clone());
                }
            }
        }
        mappings
    }

    pub(super) fn current() -> Result<Self, ProcessRunnerError> {
        #[cfg(target_os = "linux")]
        {
            use std::io::Read;
            let mut contents = String::new();
            std::fs::File::open("/proc/self/mountinfo")
                .and_then(|file| file.take(4 * 1024 * 1024 + 1).read_to_string(&mut contents))
                .map_err(|error| {
                    ProcessRunnerError::infrastructure(format!(
                        "cannot inspect mount aliases for sandbox protection: {error}"
                    ))
                })?;
            if contents.len() > 4 * 1024 * 1024 {
                return Err(ProcessRunnerError::infrastructure(
                    "sandbox mount table exceeds 4 MiB",
                ));
            }
            Self::parse(&contents)
        }
        #[cfg(not(target_os = "linux"))]
        Ok(Self::default())
    }

    #[cfg(target_os = "linux")]
    fn parse(contents: &str) -> Result<Self, ProcessRunnerError> {
        let mut mounts = Vec::new();
        for line in contents.lines() {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            if fields.len() < 10 || !fields.contains(&"-") {
                return Err(ProcessRunnerError::infrastructure(
                    "invalid sandbox mount table entry",
                ));
            }
            mounts.push(Mount {
                device: fields[2].to_owned(),
                root: decode_path(fields[3])?,
                destination: decode_path(fields[4])?,
            });
        }
        Ok(Self { mounts })
    }

    /// Projects a protected subtree through visible bind aliases without scanning files.
    pub(super) fn paths(&self, path: &Path) -> BTreeSet<PathBuf> {
        let path = resolve_bwrap_path(path);
        let mut paths = BTreeSet::from([path.clone()]);
        let Some(owner) = self
            .mounts
            .iter()
            .filter(|mount| path.starts_with(&mount.destination))
            .max_by_key(|mount| mount.destination.components().count())
        else {
            return paths;
        };
        let Ok(relative) = path.strip_prefix(&owner.destination) else {
            return paths;
        };
        let source = owner.root.join(relative);
        for mount in &self.mounts {
            if mount.device != owner.device {
                continue;
            }
            let projection = if let Ok(relative) = source.strip_prefix(&mount.root) {
                Some((mount.destination.join(relative), path.clone()))
            } else if let Ok(relative) = mount.root.strip_prefix(&source) {
                Some((mount.destination.clone(), path.join(relative)))
            } else {
                None
            };
            if let Some((destination, original)) = projection
                && same_file(&destination, &original)
            {
                paths.insert(destination);
            }
        }
        paths
    }
}

fn same_file(left: &Path, right: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (std::fs::metadata(left), std::fs::metadata(right)) {
            (Ok(left), Ok(right)) => left.dev() == right.dev() && left.ino() == right.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        left == right
    }
}

#[cfg(target_os = "linux")]
fn decode_path(value: &str) -> Result<PathBuf, ProcessRunnerError> {
    use std::os::unix::ffi::OsStringExt;
    let mut decoded = Vec::with_capacity(value.len());
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            decoded.push(byte);
            continue;
        }
        let escaped = [bytes.next(), bytes.next(), bytes.next()];
        let byte = match escaped {
            [Some(b'0'), Some(b'4'), Some(b'0')] => b' ',
            [Some(b'0'), Some(b'1'), Some(b'1')] => b'\t',
            [Some(b'0'), Some(b'1'), Some(b'2')] => b'\n',
            [Some(b'1'), Some(b'3'), Some(b'4')] => b'\\',
            _ => {
                return Err(ProcessRunnerError::infrastructure(
                    "invalid mount path escape",
                ));
            }
        };
        decoded.push(byte);
    }
    let path = PathBuf::from(std::ffi::OsString::from_vec(decoded));
    if !path.is_absolute() {
        return Err(ProcessRunnerError::infrastructure(
            "mount destination must be absolute",
        ));
    }
    Ok(path)
}
