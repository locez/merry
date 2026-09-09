use super::path_view::ActionPathView;
use merry_runtime::{PathAccess, PathAccessRule, PathAccessRuleSource, ProcessRunnerError};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

enum Mount {
    Mask {
        path: PathBuf,
        kind: crate::BwrapMaskKind,
    },
    Grant {
        path: PathBuf,
        source: PathBuf,
        access: PathAccess,
    },
}

impl Mount {
    fn path(&self) -> &Path {
        match self {
            Self::Mask { path, .. } | Self::Grant { path, .. } => path,
        }
    }
}

/// Applies restrictions after ordinary mounts, then restores only reviewed subtrees.
pub(super) fn append(
    args: &mut Vec<OsString>,
    rules: &[PathAccessRule],
    tmp_source: &Path,
    view: &ActionPathView,
) -> Result<(), ProcessRunnerError> {
    let aliases = &view.aliases;
    for rule in rules.iter().filter(|rule| {
        rule.access() == PathAccess::ReadOnly
            && !rule.review_required()
            && matches!(
                rule.source(),
                PathAccessRuleSource::TrustedGlobalConfig
                    | PathAccessRuleSource::TrustedGlobalConfigWritableCeiling
            )
    }) {
        for (path, source) in aliases.action_paths(rule.path(), tmp_source) {
            let path = view.destination(&path)?;
            args.extend([
                OsString::from("--ro-bind-try"),
                source.into_os_string(),
                path.into_os_string(),
            ]);
        }
    }
    let mut denied = BTreeSet::new();
    let mut reviewed = BTreeSet::new();
    let mut mounts = Vec::new();
    for rule in rules {
        if !rule.review_required() && rule.access() != PathAccess::Deny {
            continue;
        }
        if rule.source() == PathAccessRuleSource::ProductPrivate
            && matches!(fs::metadata(rule.path()), Err(error) if error.kind() == io::ErrorKind::NotFound)
        {
            continue;
        }
        let kind = crate::BwrapMaskKind::inspect(rule.path())?;
        let paths = aliases
            .action_paths(rule.path(), tmp_source)
            .into_keys()
            .map(|path| view.destination(&path))
            .collect::<Result<BTreeSet<_>, _>>()?;
        for path in &paths {
            mounts.push(Mount::Mask {
                path: path.clone(),
                kind,
            });
        }
        if rule.access() == PathAccess::Deny {
            denied.extend(paths);
        } else {
            reviewed.extend(paths);
        }
    }
    for rule in rules
        .iter()
        .filter(|rule| rule.source() == PathAccessRuleSource::PermissionReview)
    {
        if !super::review::required(rules, rule.path(), aliases) {
            continue;
        }
        for (path, source) in aliases.grant_paths(rule.path(), tmp_source) {
            let path = view.destination(&path)?;
            if !reviewed.iter().any(|root| path.starts_with(root))
                || denied.iter().any(|root| path.starts_with(root))
            {
                continue;
            }
            mounts.push(Mount::Grant {
                path,
                source,
                access: rule.access(),
            });
        }
    }
    mounts.sort_by_key(|mount| {
        (
            mount.path().components().count(),
            mount.path().to_path_buf(),
            matches!(mount, Mount::Grant { .. }),
        )
    });
    let mut masked_directories = BTreeSet::new();
    for mount in mounts {
        match mount {
            Mount::Mask { path, kind } => {
                kind.append(args, &path);
                if kind == crate::BwrapMaskKind::Directory {
                    masked_directories.insert(path);
                }
            }
            Mount::Grant {
                path,
                source,
                access,
            } => {
                args.extend([
                    OsString::from(if access == PathAccess::ReadWrite {
                        "--bind"
                    } else {
                        "--ro-bind"
                    }),
                    source.into_os_string(),
                    path.as_os_str().to_owned(),
                ]);
                masked_directories.remove(&path);
            }
        }
    }
    for path in masked_directories {
        args.extend([OsString::from("--remount-ro"), path.into_os_string()]);
    }
    Ok(())
}
