//! Bubblewrap argument emission from an immutable, finalized namespace.

use super::{SandboxMountError, namespace::Namespace};
use crate::BwrapMaskKind;
use merry_runtime::PathAccess;
use std::{ffi::OsString, fs, path::Path};

pub(super) fn append_args(
    namespace: &Namespace,
    args: &mut Vec<OsString>,
    replaces_file: impl Fn(&Path) -> bool,
) -> Result<(), SandboxMountError> {
    for link in namespace.links() {
        let target = fs::read_link(&link.source).map_err(|source| SandboxMountError::Io {
            path: link.source.clone(),
            source,
        })?;
        if target != link.target {
            return Err(SandboxMountError::ChangedLink {
                path: link.source.clone(),
            });
        }
    }
    let mut directories = std::collections::BTreeSet::new();
    for link in namespace.links().iter().filter(|link| link.directory) {
        let destination = namespace.destination(&link.destination)?;
        if namespace.import(&destination, None).is_none() && !namespace.denied_path(&destination) {
            directories.insert(destination);
        }
    }
    for directory in directories {
        append_parents(args, &directory);
        args.extend([OsString::from("--dir"), directory.into_os_string()]);
    }
    let mut bindings = namespace.bindings().iter().enumerate().collect::<Vec<_>>();
    bindings.sort_by_key(|(_, binding)| binding.destination.components().count());
    let mut masked_directories = Vec::new();
    for (index, binding) in bindings {
        if namespace.runtime_alias(binding) {
            continue;
        }
        if binding.access != PathAccess::Deny
            && ((!binding.directory && replaces_file(&binding.destination))
                || namespace.covered_binding(index, binding))
        {
            continue;
        }
        append_parents(args, &binding.destination);
        if binding.access == PathAccess::Deny {
            let kind =
                BwrapMaskKind::inspect(&binding.source).map_err(SandboxMountError::Protection)?;
            kind.append(args, &binding.destination);
            if kind == BwrapMaskKind::Directory {
                masked_directories.push(&binding.destination);
            }
            continue;
        }
        let flag = match (binding.access, binding.optional) {
            (PathAccess::ReadOnly, false) => "--ro-bind",
            (PathAccess::ReadOnly, true) => "--ro-bind-try",
            (PathAccess::ReadWrite, false) => "--bind",
            (PathAccess::ReadWrite, true) => "--bind-try",
            (PathAccess::Deny, _) => continue,
        };
        args.extend([
            OsString::from(flag),
            binding.source.as_os_str().to_owned(),
            binding.destination.as_os_str().to_owned(),
        ]);
    }
    let mut links = namespace.links().iter().collect::<Vec<_>>();
    links.sort_by_key(|link| link.destination.components().count());
    for link in links {
        if namespace.denied_path(&link.logical) || namespace.denied_source(&link.source) {
            continue;
        }
        append_parents(args, &link.destination);
        args.extend([
            OsString::from("--symlink"),
            link.target.as_os_str().to_owned(),
            link.destination.as_os_str().to_owned(),
        ]);
    }
    for destination in masked_directories {
        args.extend([
            OsString::from("--remount-ro"),
            destination.as_os_str().to_owned(),
        ]);
    }
    Ok(())
}

fn append_parents(args: &mut Vec<OsString>, destination: &Path) {
    if let Some(parent) = destination.parent() {
        for parent in parent
            .ancestors()
            .take_while(|path| *path != Path::new("/"))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            args.extend([OsString::from("--dir"), parent.as_os_str().to_owned()]);
        }
    }
}
