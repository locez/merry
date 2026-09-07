//! Runtime-owned capability retention, independent of host filesystem mechanics.

use super::{
    PermissionedProcessRunnerFactory, ProcessActionIntent, ProcessRunner, ProcessRunnerContext,
    ProcessRunnerError, ProcessRunnerFuture,
};
use crate::{HostIntegration, PathAccess, PathAccessRule, PermissionRequest, RequestedCapability};
use std::sync::{Arc, RwLock};

/// Backend facts that constrain retention of an approved path capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessPathGrantConstraint {
    /// No additional path constraint was found by the backend.
    Ordinary,
    /// The path falls under an explicit per-action review declaration.
    ReviewRequired,
    /// The path belongs to metadata protected independently of ordinary grants.
    ProtectedMetadata,
}

/// A normalized approved path and the constraints found while materializing it.
#[derive(Debug, Clone)]
pub struct ProcessPathGrant {
    rule: PathAccessRule,
    constraint: ProcessPathGrantConstraint,
}

/// An adapter's prepared action runner and normalized capability evidence.
pub struct PreparedProcessPermission {
    runner: Arc<dyn ProcessRunner>,
    grants: Vec<ProcessPathGrant>,
}

impl PreparedProcessPermission {
    /// Packages an already validated runner without retaining its grants.
    #[must_use]
    pub fn new(runner: Arc<dyn ProcessRunner>, grants: Vec<ProcessPathGrant>) -> Self {
        Self { runner, grants }
    }
}

impl ProcessPathGrant {
    /// Constructs backend evidence; this does not grant or retain any authority.
    #[must_use]
    pub fn new(rule: PathAccessRule, constraint: ProcessPathGrantConstraint) -> Self {
        Self { rule, constraint }
    }
}

/// Immutable capabilities retained by one runtime session.
#[derive(Debug, Clone, Default)]
pub struct ProcessSessionPermissionSnapshot {
    path_rules: Vec<PathAccessRule>,
    host_integrations: Vec<HostIntegration>,
}

impl ProcessSessionPermissionSnapshot {
    /// Returns normalized, session-scoped path grants.
    #[must_use]
    pub fn path_rules(&self) -> &[PathAccessRule] {
        &self.path_rules
    }

    /// Returns explicitly approved native host integrations.
    #[must_use]
    pub fn host_integrations(&self) -> &[HostIntegration] {
        &self.host_integrations
    }
}

/// Read-only access supplied to process adapters; it cannot record a grant.
#[derive(Debug, Clone)]
pub struct ProcessSessionPermissionView {
    state: Arc<RwLock<ProcessSessionPermissionSnapshot>>,
}

impl ProcessSessionPermissionView {
    /// Captures capabilities for a preparation, failing closed on poisoned state.
    pub fn snapshot(&self) -> Result<ProcessSessionPermissionSnapshot, ProcessRunnerError> {
        self.state.read().map(|state| state.clone()).map_err(|_| {
            ProcessRunnerError::infrastructure("runtime session permission state lock was poisoned")
        })
    }
}

/// Capability state owned by a single runtime session, never a global store.
/// Network, explicitly reviewed paths, and protected metadata remain action-only.
#[derive(Debug, Clone, Default)]
pub struct ProcessSessionPermissions {
    state: Arc<RwLock<ProcessSessionPermissionSnapshot>>,
}

impl ProcessSessionPermissions {
    /// Creates an empty, independent session.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Supplies a read-only view to an ordinary or permissioned process backend.
    #[must_use]
    pub fn view(&self) -> ProcessSessionPermissionView {
        ProcessSessionPermissionView {
            state: Arc::clone(&self.state),
        }
    }

    /// Wraps an adapter with runtime-owned approval retention. The adapter must
    /// consume this session's read-only view when constructing its runners.
    #[must_use]
    pub fn with_factory<Factory: PermissionedProcessRunnerFactory>(
        self,
        backend: Factory,
    ) -> SessionPermissionedProcessRunnerFactory<Factory> {
        SessionPermissionedProcessRunnerFactory {
            permissions: self,
            backend,
        }
    }

    fn retain(
        &self,
        request: &PermissionRequest,
        grants: Vec<ProcessPathGrant>,
    ) -> Result<(), ProcessRunnerError> {
        let mut state = self.state.write().map_err(|_| {
            ProcessRunnerError::infrastructure("runtime session permission state lock was poisoned")
        })?;
        state
            .path_rules
            .extend(grants.into_iter().filter_map(|grant| {
                (grant.constraint == ProcessPathGrantConstraint::Ordinary
                    && !grant.rule.review_required()
                    && grant.rule.access() != PathAccess::Deny)
                    .then_some(grant.rule)
            }));
        normalize_grants(&mut state.path_rules);
        state
            .host_integrations
            .extend(
                request
                    .requested()
                    .iter()
                    .filter_map(|capability| match capability {
                        RequestedCapability::HostIntegration(integration) => Some(*integration),
                        _ => None,
                    }),
            );
        state.host_integrations.sort_unstable();
        state.host_integrations.dedup();
        Ok(())
    }
}

/// Runtime decorator that records approved capabilities, not OS mount policy.
#[derive(Debug, Clone)]
pub struct SessionPermissionedProcessRunnerFactory<Factory> {
    permissions: ProcessSessionPermissions,
    backend: Factory,
}

impl<Factory> SessionPermissionedProcessRunnerFactory<Factory> {
    /// Returns the adapter for backend-specific inspection, without write access
    /// to session permission state.
    #[must_use]
    pub fn backend(&self) -> &Factory {
        &self.backend
    }
}

impl<Factory: PermissionedProcessRunnerFactory> PermissionedProcessRunnerFactory
    for SessionPermissionedProcessRunnerFactory<Factory>
{
    fn validate_request(&self, request: &PermissionRequest) -> Result<(), ProcessRunnerError> {
        self.backend.validate_request(request)
    }

    fn request_capabilities_are_satisfied(
        &self,
        request: &PermissionRequest,
    ) -> Result<bool, ProcessRunnerError> {
        self.backend.request_capabilities_are_satisfied(request)
    }

    fn runner_for(&self, request: &PermissionRequest) -> Arc<dyn ProcessRunner> {
        let result = self
            .backend
            .prepare_approved_request(request)
            .and_then(|prepared| {
                self.permissions.retain(request, prepared.grants)?;
                Ok(prepared.runner)
            });
        match result {
            Ok(runner) => runner,
            Err(error) => Arc::new(RejectedRunner(error)),
        }
    }
}

struct RejectedRunner(ProcessRunnerError);

impl ProcessRunner for RejectedRunner {
    fn run<'a>(
        &'a self,
        _intent: ProcessActionIntent,
        _context: ProcessRunnerContext,
    ) -> ProcessRunnerFuture<'a> {
        Box::pin(async { Err(self.0.clone()) })
    }
}

fn normalize_grants(rules: &mut Vec<PathAccessRule>) {
    rules.sort_by(|left, right| {
        left.path()
            .components()
            .count()
            .cmp(&right.path().components().count())
            .then_with(|| left.path().cmp(right.path()))
            .then_with(|| {
                (left.access() != PathAccess::ReadWrite)
                    .cmp(&(right.access() != PathAccess::ReadWrite))
            })
    });
    let mut retained = Vec::with_capacity(rules.len());
    for rule in rules.drain(..) {
        if !retained.iter().any(|ancestor: &PathAccessRule| {
            rule.path().starts_with(ancestor.path()) && ancestor.access().covers(rule.access())
        }) {
            retained.push(rule);
        }
    }
    *rules = retained;
}

#[cfg(test)]
mod tests;
