//! Bounded shallow link discovery using fixed source grants and namespace operations.

use super::{
    SandboxLinkIssue, SandboxMountError,
    inputs::{AdmittedSources, MountInput},
    inspect,
    namespace::{BindingOrigin, Import, Link, Namespace},
    with_suffix,
};
use crate::resolve_bwrap_path;
use merry_runtime::PathAccess;
use std::{
    collections::{BTreeSet, VecDeque},
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub(super) fn complete(
    namespace: &mut Namespace,
    inputs: &[MountInput],
    scan_roots: &[PathBuf],
) -> Result<Vec<SandboxLinkIssue>, SandboxMountError> {
    let mut completion = LinkCompletion {
        namespace,
        sources: AdmittedSources::new(inputs),
        issues: Vec::new(),
    };
    completion.preserve_source_links()?;
    completion.namespace.normalize_destinations()?;
    completion.scan_links(scan_roots)?;
    Ok(completion.issues)
}

struct LinkCompletion<'scope> {
    namespace: &'scope mut Namespace,
    sources: AdmittedSources<'scope>,
    issues: Vec<SandboxLinkIssue>,
}

const MAX_ENTRIES: usize = 250_000;
const MAX_DEPTH: usize = 128;
const MAX_SCAN_TIME: Duration = Duration::from_secs(10);

struct ScanBudget {
    entries: usize,
    started: Instant,
}

struct DependencyTarget {
    destination: PathBuf,
    scan: bool,
}

impl ScanBudget {
    fn new() -> Self {
        Self {
            entries: 0,
            started: Instant::now(),
        }
    }

    fn step(&mut self, path: &Path) -> Result<(), SandboxMountError> {
        self.entries += 1;
        let kind = if self.entries > MAX_ENTRIES {
            Some("entry count")
        } else if self.started.elapsed() > MAX_SCAN_TIME {
            Some("elapsed time")
        } else if path.components().count() > MAX_DEPTH {
            Some("path depth")
        } else {
            None
        };
        match kind {
            Some(kind) => Err(SandboxMountError::Limit {
                kind,
                path: path.to_path_buf(),
            }),
            None => Ok(()),
        }
    }
}

impl LinkCompletion<'_> {
    fn preserve_source_links(&mut self) -> Result<(), SandboxMountError> {
        let sources = self
            .namespace
            .bindings()
            .iter()
            .enumerate()
            .filter(|(_, binding)| binding.access != PathAccess::Deny)
            .map(|(index, binding)| {
                (
                    index,
                    binding.original_source.clone(),
                    binding.logical.clone(),
                )
            })
            .collect::<Vec<_>>();
        for (index, mut source, mut destination) in sources {
            if source != destination
                && let Some(parent) = source.parent()
            {
                source = resolve_bwrap_path(parent).join(source.file_name().unwrap_or_default());
            }
            let mut visited = BTreeSet::new();
            loop {
                if visited.len() > 40 || !visited.insert((source.clone(), destination.clone())) {
                    return Err(crate::SandboxPathError::Loop {
                        path: self.namespace.bindings()[index].logical.clone(),
                    }
                    .into());
                }
                let prefixes = source
                    .ancestors()
                    .zip(destination.ancestors())
                    .filter(|(_, destination)| *destination != Path::new("/"))
                    .collect::<Vec<_>>();
                let mut next = None;
                for (source_prefix, destination_prefix) in prefixes.into_iter().rev() {
                    if matches!(
                        self.namespace.import(destination_prefix, Some(index)),
                        Some(Import::Opaque)
                    ) {
                        break;
                    }
                    if !inspect(source_prefix)?
                        .is_some_and(|metadata| metadata.file_type().is_symlink())
                    {
                        continue;
                    }
                    let target =
                        fs::read_link(source_prefix).map_err(|error| SandboxMountError::Io {
                            path: source_prefix.to_path_buf(),
                            source: error,
                        })?;
                    self.record_link(source_prefix, destination_prefix, Some(index))?;
                    let source_suffix = source.strip_prefix(source_prefix).map_err(|_| {
                        SandboxMountError::Conflict {
                            path: source.clone(),
                        }
                    })?;
                    let destination_suffix =
                        destination.strip_prefix(destination_prefix).map_err(|_| {
                            SandboxMountError::Conflict {
                                path: destination.clone(),
                            }
                        })?;
                    next = Some((
                        with_suffix(
                            source_prefix
                                .parent()
                                .unwrap_or(Path::new("/"))
                                .join(&target),
                            source_suffix,
                        ),
                        with_suffix(
                            destination_prefix
                                .parent()
                                .unwrap_or(Path::new("/"))
                                .join(&target),
                            destination_suffix,
                        ),
                    ));
                    break;
                }
                let Some((next_source, next_destination)) = next else {
                    break;
                };
                source = next_source;
                destination = next_destination;
            }
        }
        Ok(())
    }

    /// Inspects direct children only; existing target coverage stops expansion.
    fn scan_links(&mut self, roots: &[PathBuf]) -> Result<(), SandboxMountError> {
        let mut pending = VecDeque::from(roots.to_vec());
        let mut visited = BTreeSet::new();
        let mut budget = ScanBudget::new();
        let mut dependencies = Vec::new();
        while let Some(directory) = pending.pop_front() {
            budget.step(&directory)?;
            let resolution = self.namespace.resolve(&directory);
            let Some(mapping) = self.dependency_result(&directory, resolution)? else {
                continue;
            };
            if !visited.insert((mapping.source.clone(), mapping.destination.clone())) {
                continue;
            }
            let access = match self.namespace.import(&mapping.destination, None) {
                Some(Import::Host(binding, _)) => binding.access,
                _ => continue,
            };
            let entries = match fs::read_dir(&mapping.source) {
                Ok(entries) => entries,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::PermissionDenied
                            | io::ErrorKind::NotFound
                            | io::ErrorKind::NotADirectory
                    ) =>
                {
                    self.issues
                        .push(SandboxLinkIssue::Unavailable { path: directory });
                    continue;
                }
                Err(source) => {
                    return Err(SandboxMountError::Io {
                        path: mapping.source,
                        source,
                    });
                }
            };
            let mut children = Vec::new();
            for entry in entries {
                budget.step(&directory)?;
                let inspection = entry.map(Some).map_err(|source| SandboxMountError::Io {
                    path: mapping.source.clone(),
                    source,
                });
                let Some(entry) = self.dependency_result(&directory, inspection)? else {
                    continue;
                };
                children.push(entry.file_name());
            }
            children.sort();
            for name in children {
                let source = mapping.source.join(&name);
                let destination = mapping.destination.join(name);
                if self.namespace.denied_path(&destination) || self.sources.denies(&source) {
                    continue;
                }
                let Some(metadata) = inspect(&source)? else {
                    self.issues
                        .push(SandboxLinkIssue::Unavailable { path: destination });
                    continue;
                };
                if metadata.file_type().is_symlink() {
                    let completion =
                        self.ensure_dependency(&source, &destination, access, &mut budget);
                    if let Some(target) = self.dependency_result(&destination, completion)? {
                        if target.scan {
                            pending.push_back(target.destination.clone());
                        }
                        dependencies.push((mapping.destination.clone(), target.destination));
                    }
                }
            }
        }
        self.merge_dependency_access(&dependencies, &mut budget)
    }

    /// Defers unavailable discovered paths, not failures of explicit mount requests.
    fn dependency_result<T>(
        &mut self,
        path: &Path,
        result: Result<Option<T>, SandboxMountError>,
    ) -> Result<Option<T>, SandboxMountError> {
        let issue = match result {
            Ok(result) => return Ok(result),
            Err(SandboxMountError::Path(crate::SandboxPathError::Loop { .. })) => {
                SandboxLinkIssue::Cycle {
                    path: path.to_path_buf(),
                }
            }
            Err(
                SandboxMountError::Io { source, .. }
                | SandboxMountError::Path(crate::SandboxPathError::Io { source, .. }),
            ) if matches!(
                source.kind(),
                io::ErrorKind::PermissionDenied
                    | io::ErrorKind::NotFound
                    | io::ErrorKind::NotADirectory
            ) =>
            {
                SandboxLinkIssue::Unavailable {
                    path: path.to_path_buf(),
                }
            }
            Err(error) => return Err(error),
        };
        self.issues.push(issue);
        Ok(None)
    }

    /// Propagates read-write demands through cached edges without directory rescans.
    fn merge_dependency_access(
        &mut self,
        dependencies: &[(PathBuf, PathBuf)],
        budget: &mut ScanBudget,
    ) -> Result<(), SandboxMountError> {
        loop {
            let mut changed = false;
            for (origin, destination) in dependencies {
                budget.step(destination)?;
                if !matches!(
                    self.namespace.import(origin, None),
                    Some(Import::Host(binding, _)) if binding.access == PathAccess::ReadWrite
                ) || self.namespace.denied_path(destination)
                {
                    continue;
                }
                let Some(Import::Host(binding, source)) = self.namespace.import(destination, None)
                else {
                    continue;
                };
                if binding.origin != BindingOrigin::Dependency
                    || binding.access.covers(PathAccess::ReadWrite)
                    || self.sources.access(&source) != Some(PathAccess::ReadWrite)
                {
                    continue;
                }
                self.namespace
                    .bind_dependency(&source, destination, PathAccess::ReadWrite)?;
                changed = true;
            }
            if !changed {
                return Ok(());
            }
        }
    }

    fn record_link(
        &mut self,
        source: &Path,
        logical: &Path,
        ignore: Option<usize>,
    ) -> Result<(), SandboxMountError> {
        if self.namespace.denied_path(logical) || self.sources.denies(&resolve_bwrap_path(source)) {
            return Ok(());
        }
        let target = fs::read_link(source).map_err(|error| SandboxMountError::Io {
            path: source.to_path_buf(),
            source: error,
        })?;
        match self.namespace.import(logical, ignore) {
            Some(Import::Opaque) => return Ok(()),
            Some(Import::Link(existing)) => {
                return if existing.target == target {
                    Ok(())
                } else {
                    Err(SandboxMountError::Conflict {
                        path: logical.to_path_buf(),
                    })
                };
            }
            Some(Import::Host(binding, existing)) if binding.exists => {
                if inspect(&existing)?.is_some_and(|metadata| metadata.file_type().is_symlink()) {
                    return Ok(());
                }
                if binding.logical != logical || binding.original_source != source {
                    return Err(SandboxMountError::Conflict {
                        path: logical.to_path_buf(),
                    });
                }
            }
            _ => {}
        }
        self.namespace.insert_link(Link {
            source: source.to_path_buf(),
            logical: logical.to_path_buf(),
            destination: logical.to_path_buf(),
            target,
            directory: source.is_dir(),
        })?;
        Ok(())
    }

    fn ensure_dependency(
        &mut self,
        source: &Path,
        logical: &Path,
        access: PathAccess,
        budget: &mut ScanBudget,
    ) -> Result<Option<DependencyTarget>, SandboxMountError> {
        let mut source = source.to_path_buf();
        let mut destination = logical.to_path_buf();
        let mut visited = BTreeSet::new();
        for _ in 0..40 {
            budget.step(&destination)?;
            if !visited.insert((source.clone(), destination.clone())) {
                self.issues.push(SandboxLinkIssue::Cycle {
                    path: logical.to_path_buf(),
                });
                return Ok(None);
            }
            let resolved = self.namespace.destination(&destination)?;
            if self.namespace.denied_path(&resolved) {
                self.issues.push(SandboxLinkIssue::UnexposedTarget {
                    link: logical.to_path_buf(),
                    target: resolved,
                });
                return Ok(None);
            }
            match self.namespace.import(&resolved, None) {
                Some(Import::Opaque) => return Ok(None),
                Some(Import::Host(binding, visible_source)) => {
                    if binding.access == PathAccess::Deny || self.sources.denies(&visible_source) {
                        self.issues.push(SandboxLinkIssue::UnexposedTarget {
                            link: logical.to_path_buf(),
                            target: resolved,
                        });
                        return Ok(None);
                    }
                    if inspect(&visible_source)?.is_none() {
                        self.issues.push(SandboxLinkIssue::Unavailable {
                            path: logical.to_path_buf(),
                        });
                        return Ok(None);
                    }
                    return Ok(Some(DependencyTarget {
                        destination: resolved,
                        scan: false,
                    }));
                }
                _ => {}
            }

            let real_source = resolve_bwrap_path(&source);
            let Some(approved_access) = self.sources.access(&real_source) else {
                self.issues.push(SandboxLinkIssue::UnexposedTarget {
                    link: logical.to_path_buf(),
                    target: resolved,
                });
                return Ok(None);
            };
            let access =
                if access == PathAccess::ReadOnly || approved_access == PathAccess::ReadOnly {
                    PathAccess::ReadOnly
                } else {
                    PathAccess::ReadWrite
                };
            let prefixes = source
                .ancestors()
                .zip(destination.ancestors())
                .filter(|(_, prefix)| *prefix != Path::new("/"))
                .map(|(source, destination)| (source.to_path_buf(), destination.to_path_buf()))
                .collect::<Vec<_>>();
            let mut next = None;
            for (source_prefix, destination_prefix) in prefixes.into_iter().rev() {
                let Some(metadata) = inspect(&source_prefix)? else {
                    continue;
                };
                if !metadata.file_type().is_symlink() {
                    continue;
                }
                let target =
                    fs::read_link(&source_prefix).map_err(|error| SandboxMountError::Io {
                        path: source_prefix.clone(),
                        source: error,
                    })?;
                self.record_link(&source_prefix, &destination_prefix, None)?;
                let source_suffix = source.strip_prefix(&source_prefix).map_err(|_| {
                    SandboxMountError::Conflict {
                        path: source.clone(),
                    }
                })?;
                let destination_suffix =
                    destination.strip_prefix(&destination_prefix).map_err(|_| {
                        SandboxMountError::Conflict {
                            path: destination.clone(),
                        }
                    })?;
                next = Some((
                    with_suffix(
                        source_prefix
                            .parent()
                            .unwrap_or(Path::new("/"))
                            .join(&target),
                        source_suffix,
                    ),
                    with_suffix(
                        destination_prefix
                            .parent()
                            .unwrap_or(Path::new("/"))
                            .join(&target),
                        destination_suffix,
                    ),
                ));
                break;
            }
            if let Some((next_source, next_destination)) = next {
                source = next_source;
                destination = next_destination;
                continue;
            }
            let Some(metadata) = inspect(&real_source)? else {
                self.issues.push(SandboxLinkIssue::Unavailable {
                    path: logical.to_path_buf(),
                });
                return Ok(None);
            };
            self.namespace
                .bind_dependency(&real_source, &resolved, access)?;
            return Ok(Some(DependencyTarget {
                destination: resolved,
                scan: metadata.is_dir(),
            }));
        }
        self.issues.push(SandboxLinkIssue::Cycle {
            path: logical.to_path_buf(),
        });
        Ok(None)
    }
}
