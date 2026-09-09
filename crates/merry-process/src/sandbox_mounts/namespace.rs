//! Namespace placement, path queries, coverage, and coordinate convergence.

use super::{
    MAX_LINKS, MAX_MOUNTS, SandboxMountError, SandboxPathSource, absolute, inputs::MountInput,
};
use crate::resolve_sandbox_path;
use merry_runtime::PathAccess;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BindingOrigin {
    Admitted,
    Dependency,
    Protection,
}

#[derive(Debug, Clone)]
pub(super) struct Binding {
    pub(super) original_source: PathBuf,
    pub(super) source: PathBuf,
    pub(super) logical: PathBuf,
    pub(super) destination: PathBuf,
    pub(super) access: PathAccess,
    pub(super) optional: bool,
    pub(super) directory: bool,
    pub(super) exists: bool,
    pub(super) origin: BindingOrigin,
}

#[derive(Debug, Clone)]
pub(super) struct Link {
    pub(super) source: PathBuf,
    pub(super) logical: PathBuf,
    pub(super) destination: PathBuf,
    pub(super) target: PathBuf,
    pub(super) directory: bool,
}

pub(super) enum Import<'scope> {
    Host(&'scope Binding, PathBuf),
    Link(&'scope Link),
    Opaque,
}

impl Binding {
    fn new(input: MountInput, origin: BindingOrigin) -> Self {
        Self {
            destination: input.logical.clone(),
            original_source: input.original_source,
            source: input.source,
            logical: input.logical,
            access: input.access,
            optional: input.optional,
            directory: input.directory,
            exists: input.exists,
            origin,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct Namespace {
    bindings: Vec<Binding>,
    opaque: Vec<PathBuf>,
    links: Vec<Link>,
}

impl Namespace {
    pub(super) fn new(inputs: &[MountInput], opaque: Vec<PathBuf>) -> Self {
        Self {
            bindings: inputs
                .iter()
                .cloned()
                .map(|input| Binding::new(input, BindingOrigin::Admitted))
                .collect(),
            opaque,
            links: Vec::new(),
        }
    }

    pub(super) fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    pub(super) fn links(&self) -> &[Link] {
        &self.links
    }

    fn add_binding(
        &mut self,
        input: MountInput,
        origin: BindingOrigin,
    ) -> Result<(), SandboxMountError> {
        if self.bindings.len() >= MAX_MOUNTS {
            return Err(SandboxMountError::Limit {
                kind: "mount count",
                path: input.logical,
            });
        }
        self.bindings.push(Binding::new(input, origin));
        Ok(())
    }

    pub(super) fn add_protection(&mut self, input: MountInput) -> Result<(), SandboxMountError> {
        self.add_binding(input, BindingOrigin::Protection)
    }

    pub(super) fn insert_link(&mut self, link: Link) -> Result<(), SandboxMountError> {
        if self.links.len() >= MAX_LINKS {
            return Err(SandboxMountError::Limit {
                kind: "link count",
                path: link.logical,
            });
        }
        self.links.push(link);
        Ok(())
    }

    pub(super) fn destination(&self, path: &Path) -> Result<PathBuf, SandboxMountError> {
        absolute(path)?;
        self.resolve_ignoring(path, None)
    }

    pub(super) fn resolve(
        &self,
        path: &Path,
    ) -> Result<Option<SandboxPathSource>, SandboxMountError> {
        self.resolve_checked(path, |_| true)
    }

    pub(super) fn resolve_checked(
        &self,
        path: &Path,
        visible: impl Fn(&Path) -> bool,
    ) -> Result<Option<SandboxPathSource>, SandboxMountError> {
        absolute(path)?;
        if !visible(path) || self.denied_path(path) {
            return Ok(None);
        }
        let destination = resolve_sandbox_path(path, |prefix| {
            if !visible(prefix) || self.denied_path(prefix) {
                return None;
            }
            match self.import(prefix, None) {
                Some(Import::Host(binding, source))
                    if binding.access != PathAccess::Deny
                        && visible(&source)
                        && !self.denied_source(&source) =>
                {
                    Some(source)
                }
                Some(Import::Link(link)) if visible(&link.source) => Some(link.source.clone()),
                _ => None,
            }
        })?;
        if !visible(&destination) || self.denied_path(&destination) {
            return Ok(None);
        }
        match self.import(&destination, None) {
            Some(Import::Host(binding, source))
                if binding.access != PathAccess::Deny
                    && binding.exists
                    && visible(&source)
                    && !self.denied_source(&source) =>
            {
                Ok(Some(SandboxPathSource::new(source, destination)))
            }
            _ => Ok(None),
        }
    }

    fn resolve_ignoring(
        &self,
        path: &Path,
        ignore: Option<usize>,
    ) -> Result<PathBuf, SandboxMountError> {
        resolve_sandbox_path(path, |prefix| match self.import(prefix, ignore) {
            Some(Import::Host(binding, source)) if binding.access != PathAccess::Deny => {
                Some(source)
            }
            Some(Import::Link(link)) => Some(link.source.clone()),
            _ => None,
        })
        .map_err(Into::into)
    }

    /// Chooses the most specific active placement; runtime roots stop host traversal.
    pub(super) fn import(&self, path: &Path, ignore: Option<usize>) -> Option<Import<'_>> {
        let binding = self
            .bindings
            .iter()
            .enumerate()
            .filter(|(index, binding)| {
                Some(*index) != ignore
                    && !self.runtime_alias(binding)
                    && ignore
                        .is_none_or(|ignored| binding.logical != self.bindings[ignored].logical)
                    && (!binding.optional || binding.exists)
                    && (path == binding.destination
                        || (binding.directory && path.starts_with(&binding.destination)))
            })
            .max_by_key(|(index, binding)| (binding.destination.components().count(), *index));
        let depth = binding.map_or(0, |(_, binding)| binding.destination.components().count());
        if self
            .opaque
            .iter()
            .any(|root| path.starts_with(root) && root.components().count() >= depth)
        {
            return Some(Import::Opaque);
        }
        if let Some(link) = self.links.iter().find(|link| link.destination == path)
            && link.destination.components().count() >= depth
        {
            return Some(Import::Link(link));
        }
        let (_, binding) = binding?;
        let relative = path.strip_prefix(&binding.destination).ok()?;
        let source = if relative.as_os_str().is_empty() {
            binding.source.clone()
        } else {
            binding.source.join(relative)
        };
        Some(Import::Host(binding, source))
    }

    pub(super) fn denied_path(&self, path: &Path) -> bool {
        self.bindings.iter().any(|binding| {
            binding.access == PathAccess::Deny
                && (path.starts_with(&binding.logical) || path.starts_with(&binding.destination))
        })
    }

    pub(super) fn denied_source(&self, source: &Path) -> bool {
        self.bindings.iter().any(|binding| {
            binding.access == PathAccess::Deny && source.starts_with(&binding.source)
        })
    }

    pub(super) fn runtime_alias(&self, binding: &Binding) -> bool {
        binding.access != PathAccess::Deny
            && binding.destination != binding.logical
            && self
                .opaque
                .iter()
                .any(|root| binding.destination.starts_with(root))
    }

    pub(super) fn covered_binding(&self, index: usize, binding: &Binding) -> bool {
        if !binding.exists {
            return false;
        }
        if self.bindings.iter().skip(index + 1).any(|other| {
            other.destination == binding.destination
                && other.source == binding.source
                && other.access == binding.access
                && other.exists
        }) {
            return true;
        }
        match self.import(&binding.destination, Some(index)) {
            Some(Import::Host(parent, source)) => {
                parent.destination != binding.destination
                    && parent.exists
                    && parent.directory
                    && parent.access == binding.access
                    && source == binding.source
            }
            _ => false,
        }
    }

    /// Converges mount and link coordinates without changing admitted access.
    pub(super) fn normalize_destinations(&mut self) -> Result<(), SandboxMountError> {
        for _ in 0..=self.bindings.len() + self.links.len() {
            let destinations = self
                .bindings
                .iter()
                .enumerate()
                .map(|(index, binding)| self.resolve_ignoring(&binding.logical, Some(index)))
                .collect::<Result<Vec<_>, _>>()?;
            let mut changed = false;
            for (binding, destination) in self.bindings.iter_mut().zip(destinations) {
                changed |= binding.destination != destination;
                binding.destination = destination;
            }
            let destinations = self
                .links
                .iter()
                .map(|link| {
                    let parent = link.logical.parent().unwrap_or(Path::new("/"));
                    let destination = self.destination(parent)?;
                    Ok(destination.join(link.logical.file_name().unwrap_or_default()))
                })
                .collect::<Result<Vec<_>, SandboxMountError>>()?;
            for (link, destination) in self.links.iter_mut().zip(destinations) {
                changed |= link.destination != destination;
                link.destination = destination;
            }
            if !changed {
                return Ok(());
            }
        }
        Err(SandboxMountError::Limit {
            kind: "namespace convergence",
            path: PathBuf::from("/"),
        })
    }

    /// Merges a derived placement or adds a narrower one using already-admitted access.
    pub(super) fn bind_dependency(
        &mut self,
        source: &Path,
        destination: &Path,
        access: PathAccess,
    ) -> Result<(), SandboxMountError> {
        if let Some(binding) = self.bindings.iter_mut().rev().find(|binding| {
            binding.origin == BindingOrigin::Dependency
                && binding.destination == destination
                && binding.source == source
        }) {
            if !binding.access.covers(access) {
                binding.access = access;
            }
            return Ok(());
        }
        self.add_binding(
            MountInput::new(source, destination, access, false)?,
            BindingOrigin::Dependency,
        )
    }
}
