//! Sandbox path planning, conflict checks, and host-directory preparation.

use crate::sandbox::{
    Error,
    host::{Host, is_valid_runtime_home_path},
    integrations::is_clean_absolute_path,
};
use merry_process::resolve_bwrap_path;
use merry_runtime::{PathAccess, PathAccessRule, PathAccessRuleSource};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

/// Paths needed by ordinary local development commands. These paths are
/// visible to the inner action sandbox by default, but remain bounded by the
/// outer sandbox and do not include the user's home-level credentials file.
pub(crate) fn default_development_path_rules(home: &Path) -> Vec<PathAccessRule> {
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".cargo"));
    let rustup_home = env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".rustup"));
    let cache_home = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".cache"));

    [
        (home.join(".local/bin"), PathAccess::ReadOnly),
        (cargo_home.join("bin"), PathAccess::ReadOnly),
        (cargo_home.join("registry"), PathAccess::ReadWrite),
        (cargo_home.join("git"), PathAccess::ReadWrite),
        (rustup_home.join("toolchains"), PathAccess::ReadOnly),
        (cache_home, PathAccess::ReadWrite),
    ]
    .into_iter()
    .filter(|(path, _)| is_clean_absolute_path(path))
    .map(|(path, access)| {
        PathAccessRule::new(
            path,
            access,
            PathAccessRuleSource::DefaultDevelopmentBaseline,
        )
    })
    .collect()
}

/// Inner action baseline for common development paths. The outer sandbox may
/// expose these paths read-write as a capability ceiling, but each action starts
/// with them read-only until a session grant upgrades the exact path.
pub(crate) fn default_inner_development_path_rules(home: &Path) -> Vec<PathAccessRule> {
    default_development_path_rules(home)
        .into_iter()
        .map(|rule| {
            PathAccessRule::new(
                rule.path().to_path_buf(),
                PathAccess::ReadOnly,
                rule.source(),
            )
        })
        .collect()
}

#[derive(Debug, Clone)]
pub(super) struct SandboxPathPlan {
    pub(super) development_rules: Vec<PathAccessRule>,
    pub(super) trusted_rules: Vec<PathAccessRule>,
    pub(super) trusted_product_rules: Vec<PathAccessRule>,
}

impl SandboxPathPlan {
    pub(super) fn new(host: &Host) -> Result<Self, Error> {
        let product_paths = [
            host.xdg_paths.state_dir().to_path_buf(),
            host.xdg_paths.managed_config_dir(),
        ];
        validate_trusted_rule_conflicts(&host.trusted_path_rules)?;
        for rule in &host.trusted_path_rules {
            if rule.access() == PathAccess::Deny
                && let Some(product_path) = product_paths
                    .iter()
                    .find(|product_path| paths_overlap(product_path, rule.path()))
            {
                return Err(Error::ProductPathConflictsWithTrustedRule {
                    product_path: product_path.clone(),
                    rule_path: rule.path().to_path_buf(),
                    access: rule.access(),
                });
            }
        }

        let (trusted_rules, trusted_product_rules) = host
            .trusted_path_rules
            .iter()
            .cloned()
            .partition::<Vec<_>, _>(|rule| !path_is_inside_product(rule.path(), &product_paths));

        Ok(Self {
            development_rules: order_path_rules(default_development_path_rules(
                host.xdg_paths.home(),
            )),
            trusted_rules: order_path_rules(trusted_rules),
            trusted_product_rules: order_path_rules(trusted_product_rules),
        })
    }
}

pub(super) fn validate_trusted_rule_conflicts(rules: &[PathAccessRule]) -> Result<(), Error> {
    for (index, rule) in rules.iter().enumerate() {
        if let Some(conflicting) = rules[index + 1..].iter().find(|other| {
            resolve_bwrap_path(other.path()) == resolve_bwrap_path(rule.path())
                && other.access() != rule.access()
        }) {
            return Err(Error::ConflictingTrustedPathRules {
                path: rule.path().to_path_buf(),
                first_access: rule.access(),
                second_access: conflicting.access(),
            });
        }
    }
    Ok(())
}

pub(super) fn order_path_rules(mut rules: Vec<PathAccessRule>) -> Vec<PathAccessRule> {
    rules.sort_by(|left, right| {
        path_depth(left.path())
            .cmp(&path_depth(right.path()))
            .then_with(|| {
                left.path()
                    .to_string_lossy()
                    .cmp(&right.path().to_string_lossy())
            })
    });
    rules.dedup_by(|left, right| left.path() == right.path() && left.access() == right.access());
    rules
}

pub(super) fn path_depth(path: &Path) -> usize {
    path.components().count()
}

pub(super) fn paths_overlap(left: &Path, right: &Path) -> bool {
    let left = resolve_bwrap_path(left);
    let right = resolve_bwrap_path(right);
    left.starts_with(&right) || right.starts_with(&left)
}

pub(super) fn path_is_inside_product(path: &Path, product_paths: &[PathBuf]) -> bool {
    let path = resolve_bwrap_path(path);
    product_paths
        .iter()
        .map(|product_path| resolve_bwrap_path(product_path))
        .any(|product_path| path.starts_with(&product_path))
}

pub(super) fn ensure_host_log_directory(host: &Host) -> Result<(), Error> {
    let Some(log_settings) = host.log_settings.as_ref() else {
        return Ok(());
    };
    let Some(log_dir) = log_settings.path.parent() else {
        return Ok(());
    };
    fs::create_dir_all(log_dir).map_err(|source| Error::LogDirectory {
        path: log_dir.to_path_buf(),
        source,
    })
}

pub(super) fn ensure_host_state_directory(host: &Host) -> Result<(), Error> {
    fs::create_dir_all(host.xdg_paths.state_dir()).map_err(|source| Error::StateDirectory {
        path: host.xdg_paths.state_dir().to_path_buf(),
        source,
    })?;
    ensure_host_log_directory(host)
}

pub(super) fn ensure_host_managed_provider_directories(host: &Host) -> Result<(), Error> {
    for path in [
        host.xdg_paths.managed_config_dir(),
        host.xdg_paths.managed_secrets_dir(),
    ] {
        fs::create_dir_all(&path).map_err(|source| Error::ManagedConfigDirectory {
            path: path.clone(),
            source,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|source| {
                Error::ManagedConfigDirectory {
                    path: path.clone(),
                    source,
                }
            })?;
        }
    }
    Ok(())
}

pub(super) fn validate_outer_paths(host: &Host) -> Result<(), Error> {
    if !is_clean_absolute_path(&host.cwd) {
        return Err(Error::InvalidWorkspacePath(
            "workspace root must be a clean absolute path",
        ));
    }

    let home = host.xdg_paths.home();
    if !is_valid_runtime_home_path(home) {
        return Err(Error::InvalidHomeLayout(
            "HOME must be a clean user path outside /tmp",
        ));
    }
    Ok(())
}
