use super::{
    environment::{BWRAP_PROGRAM, BwrapProcessEnvironment},
    sandbox::{BwrapProcessPlan, bwrap_process_plan_with_environment},
};
use crate::resolve_bwrap_path;
use merry_runtime::{
    HostIntegration, PathAccess, PathAccessRule, PathAccessRuleSource, PermissionRequest,
    PermissionedProcessRunnerFactory, ProcessActionIntent, ProcessRunner, ProcessRunnerError,
    RequestedCapability,
};
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs, io,
    path::{Component, Path, PathBuf},
    sync::{Arc, RwLock},
};

/// Host-process runner that executes each process inside bubblewrap.
///
/// This is Merry's per-action sandbox backend for Linux. It is intentionally
/// separate from the CLI outer sandbox: the outer sandbox protects the host
/// from the Merry process, while this runner protects each process action from
/// the runtime profile.
#[derive(Debug, Clone)]
pub struct BwrapProcessRunner {
    pub(super) cwd_root: PathBuf,
    pub(super) environment: BwrapProcessEnvironment,
    pub(super) network_allowed: bool,
    pub(super) path_rules: Vec<PathAccessRule>,
    pub(super) session_permissions: Option<BwrapSessionPermissions>,
    pub(super) bwrap_program: PathBuf,
    pub(super) configuration_error: Option<String>,
}

impl BwrapProcessRunner {
    /// Creates a per-action bubblewrap runner rooted at a workspace path.
    #[must_use]
    pub fn new_at_workspace_root(root: impl Into<PathBuf>) -> Self {
        Self {
            cwd_root: root.into(),
            environment: BwrapProcessEnvironment::from_current_process(),
            network_allowed: false,
            path_rules: Vec::new(),
            session_permissions: None,
            bwrap_program: PathBuf::from(BWRAP_PROGRAM),
            configuration_error: None,
        }
    }

    /// Sets the environment layout visible to child processes.
    #[must_use]
    pub fn with_environment(mut self, environment: BwrapProcessEnvironment) -> Self {
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
    pub fn with_session_permissions(mut self, permissions: BwrapSessionPermissions) -> Self {
        self.session_permissions = Some(permissions);
        self
    }

    #[cfg(test)]
    pub(super) fn with_bwrap_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.bwrap_program = program.into();
        self
    }

    pub(super) fn plan_for(
        &self,
        intent: &ProcessActionIntent,
    ) -> Result<BwrapProcessPlan, ProcessRunnerError> {
        if let Some(message) = self.configuration_error.clone() {
            return Err(ProcessRunnerError::infrastructure(message));
        }
        let environment = self.environment.validate_for_workspace(&self.cwd_root)?;
        let session_snapshot = self
            .session_permissions
            .as_ref()
            .map(BwrapSessionPermissions::snapshot)
            .transpose()?;
        let mut environment = environment;
        let mut path_rules = self.path_rules.clone();
        if let Some(snapshot) = session_snapshot {
            path_rules.extend(snapshot.path_rules);
            environment = environment.with_host_integrations(snapshot.host_integrations);
        }
        add_git_metadata_baseline_rules(&mut path_rules, &self.cwd_root)?;
        path_rules = normalize_path_rules(path_rules);
        Ok(bwrap_process_plan_with_environment(
            intent,
            &self.cwd_root,
            &environment,
            self.network_allowed,
            &path_rules,
            &self.bwrap_program,
        ))
    }
}

/// Filesystem and host-integration capabilities approved for the lifetime of
/// one runtime session.
///
/// Each process action still starts a fresh bubblewrap instance, but every
/// instance receives a snapshot of the retained path and host-integration
/// capabilities. Network access is intentionally absent from this store:
/// every network action must receive an independent reviewed runner.
/// The store is deliberately constructed and shared by one runtime backend;
/// it is not global and must not be reused across sessions.
#[derive(Debug, Clone, Default)]
pub struct BwrapSessionPermissions {
    state: Arc<RwLock<BwrapSessionPermissionState>>,
}

#[derive(Debug, Default)]
struct BwrapSessionPermissionState {
    pub(super) path_rules: Vec<PathAccessRule>,
    host_integrations: Vec<HostIntegration>,
}

#[derive(Debug, Clone, Default)]
struct BwrapSessionPermissionSnapshot {
    pub(super) path_rules: Vec<PathAccessRule>,
    host_integrations: Vec<HostIntegration>,
}

impl BwrapSessionPermissions {
    /// Creates an empty session capability store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn snapshot(&self) -> Result<BwrapSessionPermissionSnapshot, ProcessRunnerError> {
        let state = self.state.read().map_err(|_| {
            ProcessRunnerError::infrastructure(
                "session permission state lock was poisoned while building a process sandbox",
            )
        })?;
        Ok(BwrapSessionPermissionSnapshot {
            path_rules: state.path_rules.clone(),
            host_integrations: state.host_integrations.clone(),
        })
    }

    fn grant(
        &self,
        path_rules: Vec<PathAccessRule>,
        host_integrations: Vec<HostIntegration>,
    ) -> Result<(), ProcessRunnerError> {
        let mut state = self.state.write().map_err(|_| {
            ProcessRunnerError::infrastructure(
                "session permission state lock was poisoned while recording an approved capability",
            )
        })?;
        let mut combined = state.path_rules.clone();
        combined.extend(
            path_rules
                .into_iter()
                .filter(|rule| !is_git_metadata_path(rule.path())),
        );
        state.path_rules = normalize_session_path_rules(combined);
        state.host_integrations.extend(host_integrations);
        state.host_integrations.sort_unstable();
        state.host_integrations.dedup();
        Ok(())
    }
}

/// Builds a bubblewrap runner from an approved permission request.
///
/// Filesystem access stays governed by the configured workspace/root path
/// rules and the session-scoped capabilities already approved by the runtime.
/// Approved ordinary path and host-integration requests are retained by the
/// session store and applied to later actions in the same session. Network
/// requests are action-scoped and are never retained. Git metadata paths are
/// deliberately excluded from retention and must be reviewed again for every
/// action.
#[derive(Debug, Clone)]
pub struct BwrapPermissionedProcessRunnerFactory {
    pub(super) cwd_root: PathBuf,
    pub(super) environment: BwrapProcessEnvironment,
    pub(super) path_rules: Vec<PathAccessRule>,
    pub(super) session_permissions: Option<BwrapSessionPermissions>,
    pub(super) bwrap_program: PathBuf,
}

impl BwrapPermissionedProcessRunnerFactory {
    /// Creates a bubblewrap permissioned runner factory rooted at a workspace path.
    #[must_use]
    pub fn new_at_workspace_root(root: impl Into<PathBuf>) -> Self {
        Self {
            cwd_root: root.into(),
            environment: BwrapProcessEnvironment::from_current_process(),
            path_rules: Vec::new(),
            session_permissions: None,
            bwrap_program: PathBuf::from(BWRAP_PROGRAM),
        }
    }

    /// Sets the environment layout used by every materialized child runner.
    #[must_use]
    pub fn with_environment(mut self, environment: BwrapProcessEnvironment) -> Self {
        self.environment = environment;
        self
    }

    /// Installs trusted path rules shared by each materialized runner.
    #[must_use]
    pub fn with_path_rules(mut self, rules: impl IntoIterator<Item = PathAccessRule>) -> Self {
        self.path_rules = rules.into_iter().collect();
        self
    }

    /// Shares session-scoped approved capabilities with the ordinary process runner.
    #[must_use]
    pub fn with_session_permissions(mut self, permissions: BwrapSessionPermissions) -> Self {
        self.session_permissions = Some(permissions);
        self
    }

    #[cfg(test)]
    pub(super) fn with_bwrap_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.bwrap_program = program.into();
        self
    }

    fn path_rules_for_request(
        &self,
        request: &PermissionRequest,
    ) -> Result<Vec<PathAccessRule>, ProcessRunnerError> {
        let mut rules = self.path_rules.clone();
        if let Some(permissions) = &self.session_permissions {
            rules.extend(permissions.snapshot()?.path_rules);
        }
        rules.extend(self.requested_path_rules_for_request(request)?);
        add_git_metadata_baseline_rules(&mut rules, &self.cwd_root)?;
        Ok(normalize_path_rules(rules))
    }

    fn requested_path_rules_for_request(
        &self,
        request: &PermissionRequest,
    ) -> Result<Vec<PathAccessRule>, ProcessRunnerError> {
        let mut effective_rules = self.path_rules.clone();
        if let Some(permissions) = &self.session_permissions {
            effective_rules.extend(permissions.snapshot()?.path_rules);
        }
        let effective_rules = normalize_path_rules(effective_rules);
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
                    &effective_rules,
                ) {
                    Ok(access) => access,
                    Err(error) => return Some(Err(error)),
                };
                if path_rule_covers(&effective_rules, &path, effective_access) {
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

    fn grant_approved_request(
        &self,
        request: &PermissionRequest,
    ) -> Result<(), ProcessRunnerError> {
        let Some(permissions) = &self.session_permissions else {
            return Ok(());
        };
        permissions.grant(
            self.requested_path_rules_for_request(request)?,
            self.requested_host_integrations_for_request(request),
        )
    }

    pub(super) fn build_runner(&self, request: &PermissionRequest) -> BwrapProcessRunner {
        let (path_rules, configuration_error) = match self.path_rules_for_request(request) {
            Ok(path_rules) => (path_rules, None),
            Err(error) => (self.path_rules.clone(), Some(error.to_string())),
        };
        let mut environment = self.environment.clone();
        environment = environment
            .with_host_integrations(self.requested_host_integrations_for_request(request));
        if let Some(permissions) = &self.session_permissions
            && let Ok(snapshot) = permissions.snapshot()
        {
            environment = environment.with_host_integrations(snapshot.host_integrations);
        }
        let mut runner = BwrapProcessRunner::new_at_workspace_root(self.cwd_root.clone())
            .with_environment(environment)
            .with_path_rules(path_rules);
        if request.requests_network() {
            runner = runner.allow_network();
        }
        if let Some(permissions) = &self.session_permissions {
            runner = runner.with_session_permissions(permissions.clone());
        }
        runner.bwrap_program = self.bwrap_program.clone();
        runner.configuration_error = configuration_error;
        runner
    }
}

impl PermissionedProcessRunnerFactory for BwrapPermissionedProcessRunnerFactory {
    fn validate_request(&self, request: &PermissionRequest) -> Result<(), ProcessRunnerError> {
        self.environment.validate_for_workspace(&self.cwd_root)?;
        self.environment.validate_requested_host_integrations(
            &self.requested_host_integrations_for_request(request),
        )?;
        self.path_rules_for_request(request).map(|_| ())
    }

    fn request_capabilities_are_satisfied(
        &self,
        request: &PermissionRequest,
    ) -> Result<bool, ProcessRunnerError> {
        let environment = self.environment.validate_for_workspace(&self.cwd_root)?;
        let snapshot = self
            .session_permissions
            .as_ref()
            .map(BwrapSessionPermissions::snapshot)
            .transpose()?
            .unwrap_or_default();

        let requested_integrations = self.requested_host_integrations_for_request(request);
        environment.validate_requested_host_integrations(&requested_integrations)?;
        if request.requests_network() {
            return Ok(false);
        }

        let mut available_integrations = environment.host_integrations.clone();
        available_integrations.extend(snapshot.host_integrations);
        available_integrations.sort_unstable();
        available_integrations.dedup();
        if requested_integrations
            .iter()
            .any(|integration| !available_integrations.contains(integration))
        {
            return Ok(false);
        }

        let mut rules = self.path_rules.clone();
        rules.extend(snapshot.path_rules);
        let rules = normalize_path_rules(rules);
        Ok(request
            .requested()
            .iter()
            .all(|capability| match capability {
                RequestedCapability::Network => false,
                RequestedCapability::HostIntegration(_) => true,
                RequestedCapability::Path(requested) => {
                    let path = materialize_requested_path(&self.cwd_root, requested.path());
                    path_rule_covers(&rules, &path, requested.access())
                }
            }))
    }

    fn runner_for(&self, request: &PermissionRequest) -> Arc<dyn ProcessRunner> {
        let grant_error = self.grant_approved_request(request).err();
        let mut runner = self.build_runner(request);
        if let Some(error) = grant_error {
            runner.configuration_error = Some(error.to_string());
        }
        Arc::new(runner)
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
    if let Some(rule) = configured_rules
        .iter()
        .filter(|rule| {
            path_matches_rule(requested_path, rule.path()) && rule.access() == PathAccess::Deny
        })
        .max_by_key(|rule| resolved_path_depth(rule.path()))
    {
        return Err(ProcessRunnerError::infrastructure(format!(
            "requested path `{}` is denied by configured path policy `{}`",
            requested_path.display(),
            rule.path().display()
        )));
    }

    if configured_rules.iter().any(|rule| {
        path_matches_rule(requested_path, rule.path())
            && rule.access() == PathAccess::ReadOnly
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
    if requested_access == PathAccess::Deny || is_git_metadata_path(requested_path) {
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
                && matches!(
                    rule.source(),
                    PathAccessRuleSource::TrustedGlobalConfig
                        | PathAccessRuleSource::TrustedGlobalConfigWritableCeiling
                )
        })
    {
        return false;
    }

    rules
        .iter()
        .filter(|rule| {
            path_matches_rule(requested_path, rule.path())
                && rule.access() != PathAccess::Deny
                && rule.source() != PathAccessRuleSource::GitMetadataBaseline
        })
        .max_by_key(|rule| resolved_path_depth(rule.path()))
        .is_some_and(|rule| rule.access().covers(requested_access))
}

fn normalize_path_rules(rules: Vec<PathAccessRule>) -> Vec<PathAccessRule> {
    let mut merged =
        std::collections::BTreeMap::<PathBuf, (PathAccess, PathAccessRuleSource)>::new();
    for rule in rules {
        let access = if rule.source() == PathAccessRuleSource::TrustedGlobalConfigWritableCeiling
            && rule.access() == PathAccess::ReadWrite
        {
            PathAccess::ReadOnly
        } else {
            rule.access()
        };
        let entry = merged
            .entry(rule.path().to_path_buf())
            .or_insert((access, rule.source()));
        (entry.0, entry.1) = merged_path_rule(entry.0, entry.1, access, rule.source());
    }
    let mut rules = merged
        .into_iter()
        .map(|(path, (access, source))| PathAccessRule::new(path, access, source))
        .collect::<Vec<_>>();
    rules.sort_by(|left, right| {
        path_depth(left.path())
            .cmp(&path_depth(right.path()))
            .then_with(|| left.path().cmp(right.path()))
    });
    rules
}

fn normalize_session_path_rules(rules: Vec<PathAccessRule>) -> Vec<PathAccessRule> {
    let mut merged =
        std::collections::BTreeMap::<PathBuf, (PathAccess, PathAccessRuleSource)>::new();
    for rule in rules {
        let entry = merged
            .entry(rule.path().to_path_buf())
            .or_insert((rule.access(), rule.source()));
        entry.0 = session_path_access(entry.0, rule.access());
        if rule.access() == entry.0 {
            entry.1 = rule.source();
        }
    }
    let mut rules = merged
        .into_iter()
        .map(|(path, (access, source))| PathAccessRule::new(path, access, source))
        .collect::<Vec<_>>();
    rules.sort_by(|left, right| {
        path_depth(left.path())
            .cmp(&path_depth(right.path()))
            .then_with(|| left.path().cmp(right.path()))
    });

    let mut retained = Vec::with_capacity(rules.len());
    for rule in rules {
        let covered_by_ancestor = retained.iter().any(|ancestor: &PathAccessRule| {
            resolve_bwrap_path(rule.path()) != resolve_bwrap_path(ancestor.path())
                && path_matches_rule(rule.path(), ancestor.path())
                && match ancestor.access() {
                    PathAccess::Deny => true,
                    PathAccess::ReadOnly => rule.access() == PathAccess::ReadOnly,
                    PathAccess::ReadWrite => rule.access() != PathAccess::Deny,
                }
        });
        if !covered_by_ancestor {
            retained.push(rule);
        }
    }
    retained
}

fn session_path_access(left: PathAccess, right: PathAccess) -> PathAccess {
    match (left, right) {
        (PathAccess::Deny, _) | (_, PathAccess::Deny) => PathAccess::Deny,
        (PathAccess::ReadWrite, _) | (_, PathAccess::ReadWrite) => PathAccess::ReadWrite,
        (PathAccess::ReadOnly, PathAccess::ReadOnly) => PathAccess::ReadOnly,
    }
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

    let left_is_git_metadata_baseline = left_access == PathAccess::ReadOnly
        && left_source == PathAccessRuleSource::GitMetadataBaseline;
    let right_is_git_metadata_baseline = right_access == PathAccess::ReadOnly
        && right_source == PathAccessRuleSource::GitMetadataBaseline;
    if left_is_git_metadata_baseline
        && right_source == PathAccessRuleSource::PermissionReview
        && right_access == PathAccess::ReadWrite
    {
        return (right_access, right_source);
    }
    if right_is_git_metadata_baseline
        && left_source == PathAccessRuleSource::PermissionReview
        && left_access == PathAccess::ReadWrite
    {
        return (left_access, left_source);
    }
    if left_is_git_metadata_baseline {
        return (left_access, left_source);
    }
    if right_is_git_metadata_baseline {
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
    resolve_bwrap_path(path)
        .components()
        .any(|component| matches!(component, Component::Normal(name) if name == OsStr::new(".git")))
}

fn path_matches_rule(path: &Path, rule_path: &Path) -> bool {
    let path = resolve_bwrap_path(path);
    let rule_path = resolve_bwrap_path(rule_path);
    path == rule_path || path.starts_with(&rule_path)
}

fn resolved_path_depth(path: &Path) -> usize {
    path_depth(&resolve_bwrap_path(path))
}

fn git_metadata_baseline_rule(path: impl Into<PathBuf>) -> PathAccessRule {
    PathAccessRule::new(
        path,
        PathAccess::ReadOnly,
        PathAccessRuleSource::GitMetadataBaseline,
    )
}

fn add_git_metadata_baseline_rules(
    rules: &mut Vec<PathAccessRule>,
    workspace_root: &Path,
) -> Result<(), ProcessRunnerError> {
    let reviewed_git_write_paths = rules
        .iter()
        .filter(|rule| {
            rule.source() == PathAccessRuleSource::PermissionReview
                && rule.access() == PathAccess::ReadWrite
                && is_git_metadata_path(rule.path())
        })
        .map(|rule| rule.path().to_path_buf())
        .collect::<BTreeSet<_>>();
    let mut git_metadata_roots = BTreeSet::from([workspace_root.to_path_buf()]);
    git_metadata_roots.extend(
        rules
            .iter()
            .filter(|rule| rule.access() == PathAccess::ReadWrite)
            .map(|rule| rule.path().to_path_buf()),
    );

    for root in git_metadata_roots {
        let metadata_path = if is_git_metadata_path(&root) {
            root
        } else {
            root.join(".git")
        };
        if !git_metadata_path_exists(&metadata_path)?
            || reviewed_git_write_paths.contains(&metadata_path)
        {
            continue;
        }
        rules.push(git_metadata_baseline_rule(metadata_path));
    }
    Ok(())
}

fn git_metadata_path_exists(path: &Path) -> Result<bool, ProcessRunnerError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(source)
            if matches!(
                source.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) =>
        {
            Ok(false)
        }
        Err(source) => Err(ProcessRunnerError::infrastructure(format!(
            "failed to inspect Git metadata path `{}`: {source}",
            path.display()
        ))),
    }
}

fn path_depth(path: &Path) -> usize {
    path.components().count()
}
