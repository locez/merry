use super::{environment::WorkspaceProcessEnvironment, review};
use crate::resolve_host_path;
use merry_runtime::{
    HostIntegration, PathAccess, PathAccessRule, PathAccessRuleSource, PermissionRequest,
    PermissionedProcessRunnerFactory, PreparedProcessPermission, ProcessActionIntent,
    ProcessPathGrant, ProcessPathGrantConstraint, ProcessRunner, ProcessRunnerError,
    ProcessSessionPermissionView, RequestedCapability, SessionPermissionedProcessRunnerFactory,
};
use std::{
    ffi::OsStr,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

pub use merry_runtime::ProcessSessionPermissions;

/// Host-process runner that confines each process action.
///
/// Linux applies Landlock and, unless the action was granted network, a seccomp
/// filter. macOS applies a Seatbelt profile. The Merry process itself stays
/// unrestricted.
#[derive(Debug, Clone)]
pub struct WorkspaceProcessRunner {
    pub(super) cwd_root: PathBuf,
    pub(super) environment: WorkspaceProcessEnvironment,
    pub(super) network_allowed: bool,
    pub(super) path_rules: Vec<PathAccessRule>,
    pub(super) session_permissions: Option<ProcessSessionPermissionView>,
    pub(super) configuration_error: Option<String>,
    workspace_writable: bool,
}

impl WorkspaceProcessRunner {
    /// Creates a per-action workspace runner rooted at a workspace path.
    #[must_use]
    pub fn new_at_workspace_root(root: impl Into<PathBuf>) -> Self {
        Self {
            cwd_root: root.into(),
            environment: WorkspaceProcessEnvironment::from_current_process(),
            network_allowed: false,
            path_rules: Vec::new(),
            session_permissions: None,
            configuration_error: None,
            workspace_writable: true,
        }
    }

    /// Keeps the workspace readable and not writable.
    ///
    /// Baseline paths such as `/tmp` stay writable. This does not turn the
    /// sandbox off.
    #[must_use]
    pub fn read_only_workspace(mut self) -> Self {
        self.workspace_writable = false;
        self
    }

    /// Sets the environment layout visible to child processes.
    #[must_use]
    pub fn with_environment(mut self, environment: WorkspaceProcessEnvironment) -> Self {
        self.environment = environment;
        self
    }

    /// Allows network access for child process actions.
    #[must_use]
    pub fn allow_network(mut self) -> Self {
        self.network_allowed = true;
        self
    }

    /// Installs trusted path rules for child process actions.
    #[must_use]
    pub fn with_path_rules(mut self, rules: impl IntoIterator<Item = PathAccessRule>) -> Self {
        self.path_rules = rules.into_iter().collect();
        self
    }

    /// Shares session-scoped approved capabilities with the permissioned runner factory.
    #[must_use]
    pub fn with_session_permissions(mut self, permissions: ProcessSessionPermissions) -> Self {
        self.session_permissions = Some(permissions.view());
        self
    }

    pub(super) fn plan_for(
        &self,
        intent: &ProcessActionIntent,
    ) -> Result<crate::confinement::SandboxLaunch, ProcessRunnerError> {
        if let Some(message) = self.configuration_error.clone() {
            return Err(ProcessRunnerError::infrastructure(message));
        }
        let environment = self.environment.validate_for_workspace(&self.cwd_root)?;
        let session_snapshot = self
            .session_permissions
            .as_ref()
            .map(ProcessSessionPermissionView::snapshot)
            .transpose()?;
        let mut environment = environment;
        let mut path_rules = self.path_rules.clone();
        if let Some(snapshot) = session_snapshot {
            path_rules.extend(snapshot.path_rules().iter().cloned());
            environment =
                environment.with_host_integrations(snapshot.host_integrations().iter().copied());
        }
        path_rules = normalize_path_rules(path_rules);
        crate::confinement::prepare_launch(
            intent,
            &self.cwd_root,
            &environment,
            self.network_allowed,
            &path_rules,
            self.workspace_writable,
        )
    }
}

/// Builds a workspace runner from an approved permission request.
///
/// Filesystem access stays governed by the configured path rules and the
/// session-scoped capabilities already approved by the runtime. Approved
/// ordinary path and host-integration requests are retained for later actions
/// in the same session. Network requests and paths marked for review stay
/// action-scoped.
#[derive(Debug, Clone)]
pub struct WorkspaceProcessRunnerFactory {
    pub(super) cwd_root: PathBuf,
    pub(super) environment: WorkspaceProcessEnvironment,
    pub(super) path_rules: Vec<PathAccessRule>,
    network_requests_allowed: bool,
    pub(super) session_permissions: Option<ProcessSessionPermissionView>,
    workspace_writable: bool,
}

impl WorkspaceProcessRunnerFactory {
    /// Creates a workspace-sandbox runner factory rooted at a workspace path.
    #[must_use]
    pub fn new_at_workspace_root(root: impl Into<PathBuf>) -> Self {
        Self {
            cwd_root: root.into(),
            environment: WorkspaceProcessEnvironment::from_current_process(),
            path_rules: Vec::new(),
            network_requests_allowed: true,
            session_permissions: None,
            workspace_writable: true,
        }
    }

    /// Keeps the workspace readable and not writable for every child runner.
    #[must_use]
    pub fn read_only_workspace(mut self) -> Self {
        self.workspace_writable = false;
        self
    }

    /// Sets the environment layout used by every materialized child runner.
    #[must_use]
    pub fn with_environment(mut self, environment: WorkspaceProcessEnvironment) -> Self {
        self.environment = environment;
        self
    }

    /// Installs trusted path rules shared by each materialized runner.
    #[must_use]
    pub fn with_path_rules(mut self, rules: impl IntoIterator<Item = PathAccessRule>) -> Self {
        self.path_rules = rules.into_iter().collect();
        self
    }

    /// Sets whether network capability requests can proceed to review.
    ///
    /// Defaults to true without preauthorizing access. False rejects requests
    /// during validation and materialization, including direct runner creation.
    #[must_use]
    pub fn with_network_requests_allowed(mut self, allowed: bool) -> Self {
        self.network_requests_allowed = allowed;
        self
    }

    /// Attaches a read-only capability view and returns the runtime-owned
    /// retention decorator. Configure backend-specific options before attaching it.
    #[must_use]
    pub fn with_session_permissions(
        mut self,
        permissions: ProcessSessionPermissions,
    ) -> SessionPermissionedProcessRunnerFactory<Self> {
        self.session_permissions = Some(permissions.view());
        permissions.with_factory(self)
    }

    fn requested_path_rules_for_request(
        &self,
        request: &PermissionRequest,
        effective_rules: &[PathAccessRule],
    ) -> Result<Vec<PathAccessRule>, ProcessRunnerError> {
        request
            .requested()
            .iter()
            .filter_map(|capability| {
                let RequestedCapability::Path(requested) = capability else {
                    return None;
                };
                let path = materialize_requested_path(&self.cwd_root, requested.path());
                let effective_access = match effective_requested_path_access(
                    &path,
                    requested.access(),
                    effective_rules,
                ) {
                    Ok(access) => access,
                    Err(error) => return Some(Err(error)),
                };
                let needs_review = review::required(&self.path_rules, &path);
                if !needs_review && path_rule_covers(effective_rules, &path, effective_access) {
                    return None;
                }
                Some(Ok(PathAccessRule::new(
                    path,
                    effective_access,
                    PathAccessRuleSource::PermissionReview,
                )))
            })
            .collect()
    }

    fn requested_host_integrations_for_request(
        &self,
        request: &PermissionRequest,
    ) -> Vec<HostIntegration> {
        request
            .requested()
            .iter()
            .filter_map(|capability| match capability {
                RequestedCapability::HostIntegration(integration) => Some(*integration),
                _ => None,
            })
            .collect()
    }

    pub(super) fn build_runner(&self, request: &PermissionRequest) -> WorkspaceProcessRunner {
        match self.prepare_runner(request) {
            Ok((runner, _)) => runner,
            Err(error) => {
                let mut runner = WorkspaceProcessRunner::new_at_workspace_root(&self.cwd_root);
                runner.configuration_error = Some(error.to_string());
                runner
            }
        }
    }

    fn prepare_runner(
        &self,
        request: &PermissionRequest,
    ) -> Result<(WorkspaceProcessRunner, Vec<ProcessPathGrant>), ProcessRunnerError> {
        self.validate_network_request(request)?;
        let mut environment = self.environment.validate_for_workspace(&self.cwd_root)?;
        let integrations = self.requested_host_integrations_for_request(request);
        environment.validate_requested_host_integrations(&integrations)?;
        let snapshot = self
            .session_permissions
            .as_ref()
            .map(ProcessSessionPermissionView::snapshot)
            .transpose()?
            .unwrap_or_default();
        let mut path_rules = self.path_rules.clone();
        path_rules.extend(snapshot.path_rules().iter().cloned());
        path_rules = normalize_path_rules(path_rules);
        let requested_rules = self.requested_path_rules_for_request(request, &path_rules)?;
        let grants = requested_rules
            .iter()
            .map(|rule| {
                let constraint = if review::required(&self.path_rules, rule.path()) {
                    ProcessPathGrantConstraint::ReviewRequired
                } else if is_git_metadata_path(rule.path()) {
                    ProcessPathGrantConstraint::ProtectedMetadata
                } else {
                    ProcessPathGrantConstraint::Ordinary
                };
                ProcessPathGrant::new(
                    PathAccessRule::new(
                        resolve_host_path(rule.path()),
                        rule.access(),
                        rule.source(),
                    ),
                    constraint,
                )
            })
            .collect();
        path_rules.extend(requested_rules);
        path_rules = normalize_path_rules(path_rules);
        environment = environment
            .with_host_integrations(integrations)
            .with_host_integrations(snapshot.host_integrations().iter().copied());
        let mut runner = WorkspaceProcessRunner::new_at_workspace_root(self.cwd_root.clone())
            .with_environment(environment)
            .with_path_rules(path_rules);
        if !self.workspace_writable {
            runner = runner.read_only_workspace();
        }
        if request.requests_network() {
            runner = runner.allow_network();
        }
        runner.session_permissions = self.session_permissions.clone();
        Ok((runner, grants))
    }

    /// Enforces the capability ceiling before review, grants, or runner construction.
    fn validate_network_request(
        &self,
        request: &PermissionRequest,
    ) -> Result<(), ProcessRunnerError> {
        if request.requests_network() && !self.network_requests_allowed {
            return Err(ProcessRunnerError::infrastructure(
                "network requests are disabled by the configured process capability ceiling",
            ));
        }
        Ok(())
    }
}

impl PermissionedProcessRunnerFactory for WorkspaceProcessRunnerFactory {
    fn validate_request(&self, request: &PermissionRequest) -> Result<(), ProcessRunnerError> {
        self.prepare_runner(request).map(|_| ())
    }

    fn request_capabilities_are_satisfied(
        &self,
        request: &PermissionRequest,
    ) -> Result<bool, ProcessRunnerError> {
        self.validate_network_request(request)?;
        let environment = self.environment.validate_for_workspace(&self.cwd_root)?;
        let snapshot = self
            .session_permissions
            .as_ref()
            .map(ProcessSessionPermissionView::snapshot)
            .transpose()?
            .unwrap_or_default();

        let requested_integrations = self.requested_host_integrations_for_request(request);
        environment.validate_requested_host_integrations(&requested_integrations)?;
        if request.requests_network() {
            return Ok(false);
        }

        let mut available_integrations = environment.host_integrations.clone();
        available_integrations.extend(snapshot.host_integrations().iter().copied());
        available_integrations.sort_unstable();
        available_integrations.dedup();
        if requested_integrations
            .iter()
            .any(|integration| !available_integrations.contains(integration))
        {
            return Ok(false);
        }

        let mut rules = self.path_rules.clone();
        rules.extend(snapshot.path_rules().iter().cloned());
        let rules = normalize_path_rules(rules);
        for capability in request.requested() {
            let satisfied = match capability {
                RequestedCapability::Network => false,
                RequestedCapability::HostIntegration(_) => true,
                RequestedCapability::Path(requested) => {
                    let path = materialize_requested_path(&self.cwd_root, requested.path());
                    !review::required(&self.path_rules, &path)
                        && path_rule_covers(&rules, &path, requested.access())
                }
            };
            if !satisfied {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn runner_for(&self, request: &PermissionRequest) -> Arc<dyn ProcessRunner> {
        Arc::new(self.build_runner(request))
    }

    fn prepare_approved_request(
        &self,
        request: &PermissionRequest,
    ) -> Result<PreparedProcessPermission, ProcessRunnerError> {
        let (runner, grants) = self.prepare_runner(request)?;
        Ok(PreparedProcessPermission::new(Arc::new(runner), grants))
    }
}

fn materialize_requested_path(root: &Path, requested: &str) -> PathBuf {
    let path = Path::new(requested);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn effective_requested_path_access(
    requested_path: &Path,
    requested_access: PathAccess,
    configured_rules: &[PathAccessRule],
) -> Result<PathAccess, ProcessRunnerError> {
    let requested_path = resolve_host_path(requested_path);
    let applies = |rule: &&PathAccessRule| {
        let root = resolve_host_path(rule.path());
        requested_path == root || requested_path.starts_with(&root)
    };
    if let Some(rule) = configured_rules
        .iter()
        .filter(applies)
        .filter(|rule| rule.access() == PathAccess::Deny)
        .max_by_key(|rule| resolved_path_depth(rule.path()))
    {
        return Err(ProcessRunnerError::infrastructure(format!(
            "requested path `{}` is denied by configured path policy `{}`",
            requested_path.display(),
            rule.path().display()
        )));
    }

    if configured_rules.iter().filter(applies).any(|rule| {
        rule.access() == PathAccess::ReadOnly
            && rule.source() == PathAccessRuleSource::TrustedGlobalConfig
    }) {
        // Explicit global read-only rules are a hard ceiling. Git metadata
        // baselines are intentionally different: a reviewed action may
        // temporarily overlay them, but the grant is never session-persistent.
        return Ok(PathAccess::ReadOnly);
    }

    Ok(requested_access)
}

fn path_rule_covers(
    rules: &[PathAccessRule],
    requested_path: &Path,
    requested_access: PathAccess,
) -> bool {
    if requested_access == PathAccess::Deny {
        return false;
    }
    if rules.iter().any(|rule| {
        path_matches_rule(requested_path, rule.path()) && rule.access() == PathAccess::Deny
    }) {
        return false;
    }
    if requested_access == PathAccess::ReadWrite
        && rules.iter().any(|rule| {
            path_matches_rule(requested_path, rule.path())
                && rule.access() == PathAccess::ReadOnly
                && rule.source() == PathAccessRuleSource::TrustedGlobalConfig
        })
    {
        return false;
    }

    rules
        .iter()
        .filter(|rule| {
            path_matches_rule(requested_path, rule.path()) && rule.access() != PathAccess::Deny
        })
        .max_by_key(|rule| resolved_path_depth(rule.path()))
        .is_some_and(|rule| rule.access().covers(requested_access))
}

fn normalize_path_rules(rules: Vec<PathAccessRule>) -> Vec<PathAccessRule> {
    let has_review = rules.iter().any(PathAccessRule::review_required);
    let mut action_rules = Vec::new();
    let mut merged =
        std::collections::BTreeMap::<PathBuf, (PathAccess, PathAccessRuleSource)>::new();
    for rule in rules {
        if rule.review_required()
            || (has_review && rule.source() == PathAccessRuleSource::PermissionReview)
        {
            action_rules.push(rule);
            continue;
        }
        let access = rule.access();
        let entry = merged
            .entry(rule.path().to_path_buf())
            .or_insert((access, rule.source()));
        (entry.0, entry.1) = merged_path_rule(entry.0, entry.1, access, rule.source());
    }
    let mut rules = merged
        .into_iter()
        .map(|(path, (access, source))| PathAccessRule::new(path, access, source))
        .collect::<Vec<_>>();
    rules.extend(action_rules);
    rules.sort_by(|left, right| {
        path_depth(left.path())
            .cmp(&path_depth(right.path()))
            .then_with(|| left.path().cmp(right.path()))
    });
    rules
}

fn merged_path_rule(
    left_access: PathAccess,
    left_source: PathAccessRuleSource,
    right_access: PathAccess,
    right_source: PathAccessRuleSource,
) -> (PathAccess, PathAccessRuleSource) {
    if left_access == PathAccess::Deny {
        return (left_access, left_source);
    }
    if right_access == PathAccess::Deny {
        return (right_access, right_source);
    }

    let left_is_hard_read_only = left_access == PathAccess::ReadOnly
        && left_source == PathAccessRuleSource::TrustedGlobalConfig;
    let right_is_hard_read_only = right_access == PathAccess::ReadOnly
        && right_source == PathAccessRuleSource::TrustedGlobalConfig;
    if left_is_hard_read_only {
        return (left_access, left_source);
    }
    if right_is_hard_read_only {
        return (right_access, right_source);
    }

    let left_is_reviewed_write = left_access == PathAccess::ReadWrite
        && left_source == PathAccessRuleSource::PermissionReview;
    let right_is_reviewed_write = right_access == PathAccess::ReadWrite
        && right_source == PathAccessRuleSource::PermissionReview;
    if left_is_reviewed_write {
        return (left_access, left_source);
    }
    if right_is_reviewed_write {
        return (right_access, right_source);
    }

    match (left_access, right_access) {
        (PathAccess::ReadWrite, _) => (left_access, left_source),
        (_, PathAccess::ReadWrite) => (right_access, right_source),
        _ => (PathAccess::ReadOnly, left_source),
    }
}

pub(super) fn is_git_metadata_path(path: &Path) -> bool {
    resolve_host_path(path)
        .components()
        .any(|component| matches!(component, Component::Normal(name) if name == OsStr::new(".git")))
}

fn path_matches_rule(path: &Path, rule_path: &Path) -> bool {
    let path = resolve_host_path(path);
    let rule_path = resolve_host_path(rule_path);
    path == rule_path || path.starts_with(&rule_path)
}

fn resolved_path_depth(path: &Path) -> usize {
    path_depth(&resolve_host_path(path))
}

fn path_depth(path: &Path) -> usize {
    path.components().count()
}
