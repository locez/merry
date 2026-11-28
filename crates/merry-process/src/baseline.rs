//! Default local development toolchain and cache baseline rules for isolated
//! process actions.

use merry_core::host_paths::is_clean_absolute_path;
use merry_runtime::{PathAccess, PathAccessRule, PathAccessRuleSource};
use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
};

/// Paths ordinary local development commands can use without another grant.
///
/// Covers common Rust, Python, Node/JS, Go, Nix, and polyglot version-manager
/// toolchains and build caches. Toolchain binaries, interpreters, and
/// non-secret config files stay read-only; package and compiler caches are
/// writable. Optional language caches are only marked writable when explicitly
/// configured via environment variables or when their toolchain/cache tree
/// exists, so Merry does not create empty directories in `$HOME` for unused
/// languages. Credentials under `$HOME` (such as `~/.cargo/credentials.toml`,
/// `~/.npmrc`, `~/.pypirc`, `~/.netrc`, `~/.ssh`, `~/.aws`, `~/.gnupg`) are
/// never included.
#[must_use]
pub fn default_development_path_rules(home: &Path) -> Vec<PathAccessRule> {
    default_development_path_rules_with_env(home, |name| env::var_os(name))
}

/// Pure testable variant of [`default_development_path_rules`].
#[must_use]
pub fn default_development_path_rules_with_env(
    home: &Path,
    env_var: impl Fn(&str) -> Option<OsString>,
) -> Vec<PathAccessRule> {
    let env_path = |name: &str| {
        env_var(name)
            .map(PathBuf::from)
            .filter(|path| is_clean_absolute_path(path))
    };

    let cargo_home = env_path("CARGO_HOME").unwrap_or_else(|| home.join(".cargo"));
    let rustup_home = env_path("RUSTUP_HOME").unwrap_or_else(|| home.join(".rustup"));
    let cache_home = env_path("XDG_CACHE_HOME").unwrap_or_else(|| home.join(".cache"));
    let data_home = env_path("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share"));

    let mut rules = vec![
        // User binaries & Nix profile
        (home.join(".local/bin"), PathAccess::ReadOnly),
        (home.join(".nix-profile"), PathAccess::ReadOnly),
        // Rust (`credentials.toml` stays excluded)
        (cargo_home.join("bin"), PathAccess::ReadOnly),
        (cargo_home.join("config.toml"), PathAccess::ReadOnly),
        (cargo_home.join("config"), PathAccess::ReadOnly),
        (cargo_home.join("registry"), PathAccess::ReadWrite),
        (cargo_home.join("git"), PathAccess::ReadWrite),
        (rustup_home.join("settings.toml"), PathAccess::ReadOnly),
        (rustup_home.join("toolchains"), PathAccess::ReadOnly),
        // Shared XDG cache
        (cache_home, PathAccess::ReadWrite),
        // Python toolchains & managed interpreters (never all of `$XDG_DATA_HOME`)
        (data_home.join("uv"), PathAccess::ReadOnly),
        (data_home.join("pipx"), PathAccess::ReadOnly),
        (data_home.join("pypoetry"), PathAccess::ReadOnly),
        // Polyglot version managers (`mise`, `asdf`)
        (data_home.join("mise/installs"), PathAccess::ReadOnly),
        (data_home.join("mise/shims"), PathAccess::ReadOnly),
    ];

    let pyenv_root = env_path("PYENV_ROOT").unwrap_or_else(|| home.join(".pyenv"));
    rules.push((pyenv_root.join("versions"), PathAccess::ReadOnly));
    rules.push((pyenv_root.join("shims"), PathAccess::ReadOnly));

    if let Some(uv_python) = env_path("UV_PYTHON_INSTALL_DIR") {
        rules.push((uv_python, PathAccess::ReadOnly));
    }
    if let Some(poetry_home) = env_path("POETRY_HOME") {
        rules.push((poetry_home, PathAccess::ReadOnly));
    }
    if let Some(mise_data) = env_path("MISE_DATA_DIR") {
        rules.push((mise_data.join("installs"), PathAccess::ReadOnly));
        rules.push((mise_data.join("shims"), PathAccess::ReadOnly));
    }
    let asdf_data = env_path("ASDF_DATA_DIR").unwrap_or_else(|| home.join(".asdf"));
    rules.push((asdf_data.join("installs"), PathAccess::ReadOnly));
    rules.push((asdf_data.join("shims"), PathAccess::ReadOnly));

    // Node / JS / TS toolchains (`~/.npmrc` stays excluded)
    let nvm_dir = env_path("NVM_DIR").unwrap_or_else(|| home.join(".nvm"));
    rules.push((nvm_dir.join("versions"), PathAccess::ReadOnly));

    if let Some(fnm_dir) = env_path("FNM_DIR") {
        rules.push((fnm_dir, PathAccess::ReadOnly));
    } else {
        rules.push((data_home.join("fnm"), PathAccess::ReadOnly));
        rules.push((home.join(".fnm"), PathAccess::ReadOnly));
        #[cfg(target_os = "macos")]
        rules.push((
            home.join("Library/Application Support/fnm"),
            PathAccess::ReadOnly,
        ));
    }

    let volta_home = env_path("VOLTA_HOME").unwrap_or_else(|| home.join(".volta"));
    rules.push((volta_home.join("bin"), PathAccess::ReadOnly));
    rules.push((volta_home.join("tools"), PathAccess::ReadOnly));

    let bun_env = env_path("BUN_INSTALL");
    let bun_install = bun_env.clone().unwrap_or_else(|| home.join(".bun"));
    rules.push((bun_install.join("bin"), PathAccess::ReadOnly));
    if bun_env.is_some() || bun_install.exists() {
        rules.push((bun_install.join("install/cache"), PathAccess::ReadWrite));
    }

    let pnpm_home = env_path("PNPM_HOME").unwrap_or_else(|| {
        #[cfg(target_os = "macos")]
        {
            home.join("Library/pnpm")
        }
        #[cfg(not(target_os = "macos"))]
        {
            data_home.join("pnpm")
        }
    });
    rules.push((pnpm_home.clone(), PathAccess::ReadOnly));
    if pnpm_home.join("store").exists() {
        rules.push((pnpm_home.join("store"), PathAccess::ReadWrite));
    }

    if let Some(npm_cache) = env_path("NPM_CONFIG_CACHE") {
        rules.push((npm_cache, PathAccess::ReadWrite));
    } else if home.join(".npm").exists() {
        rules.push((home.join(".npm/_cacache"), PathAccess::ReadWrite));
    }

    // Go toolchain & module cache
    let gopath_env = env_path("GOPATH");
    let gopath = gopath_env.clone().unwrap_or_else(|| home.join("go"));
    rules.push((gopath.join("bin"), PathAccess::ReadOnly));
    if let Some(gomodcache) = env_path("GOMODCACHE") {
        rules.push((gomodcache, PathAccess::ReadWrite));
    } else if gopath_env.is_some() || gopath.exists() {
        rules.push((gopath.join("pkg/mod"), PathAccess::ReadWrite));
    } else {
        rules.push((gopath.join("pkg/mod"), PathAccess::ReadOnly));
    }

    // Explicit env-configured compiler/package caches
    for var in ["GOCACHE", "UV_CACHE_DIR", "PIP_CACHE_DIR"] {
        if let Some(path) = env_path(var) {
            rules.push((path, PathAccess::ReadWrite));
        }
    }

    // macOS CLI dev caches live under `~/Library/Caches/<tool>` rather than
    // `$XDG_CACHE_HOME`. Grant only explicit dev subdirectories, never the
    // whole `~/Library/Caches` tree.
    #[cfg(target_os = "macos")]
    {
        let library_caches = home.join("Library/Caches");
        if library_caches.is_dir() {
            for subdir in ["go-build", "pip", "uv", "pnpm", "Yarn", "node-gyp", "clang"] {
                rules.push((library_caches.join(subdir), PathAccess::ReadWrite));
            }
        }
    }

    // Windows CLI dev toolchains and caches live under `%LOCALAPPDATA%` /
    // `%APPDATA%` / `%USERPROFILE%\scoop`. Grant only explicit dev
    // subdirectories, never the whole AppData tree.
    #[cfg(windows)]
    {
        if let Some(local_appdata) = env_path("LOCALAPPDATA") {
            for subdir in [
                "go-build",
                "pip",
                "uv",
                "pnpm",
                "Yarn",
                "node-gyp",
                "npm-cache",
            ] {
                let candidate = local_appdata.join(subdir);
                if candidate.exists() {
                    rules.push((candidate, PathAccess::ReadWrite));
                }
            }
            for subdir in ["fnm", "Programs\\Python", "Programs\\Git"] {
                let candidate = local_appdata.join(subdir);
                if candidate.exists() {
                    rules.push((candidate, PathAccess::ReadOnly));
                }
            }
        }
        let scoop_shims = home.join("scoop").join("shims");
        if scoop_shims.exists() {
            rules.push((scoop_shims, PathAccess::ReadOnly));
        }
    }

    rules
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

#[cfg(test)]
mod tests {
    use super::default_development_path_rules_with_env;
    use merry_runtime::{PathAccess, PathAccessRule};
    use std::{fs, path::PathBuf};

    #[test]
    fn default_development_baseline_covers_polyglot_toolchains_and_excludes_credentials() {
        let home = tempfile::tempdir().expect("temp home");
        let home_path = home.path();
        fs::create_dir_all(home_path.join("go/pkg/mod")).expect("go dir");
        fs::create_dir_all(home_path.join(".npm")).expect("npm dir");
        fs::create_dir_all(home_path.join(".bun")).expect("bun dir");
        #[cfg(target_os = "macos")]
        fs::create_dir_all(home_path.join("Library/Caches")).expect("macos caches dir");

        let rules = default_development_path_rules_with_env(home_path, |_| None);
        let access_for = |path: PathBuf| {
            rules
                .iter()
                .find(|rule| rule.path() == path)
                .map(PathAccessRule::access)
        };

        assert_eq!(
            access_for(home_path.join(".rustup/settings.toml")),
            Some(PathAccess::ReadOnly)
        );
        assert_eq!(
            access_for(home_path.join(".cargo/config.toml")),
            Some(PathAccess::ReadOnly)
        );
        assert_eq!(
            access_for(home_path.join(".local/share/uv")),
            Some(PathAccess::ReadOnly)
        );
        assert_eq!(
            access_for(home_path.join(".nvm/versions")),
            Some(PathAccess::ReadOnly)
        );
        assert_eq!(
            access_for(home_path.join(".nix-profile")),
            Some(PathAccess::ReadOnly)
        );
        assert_eq!(
            access_for(home_path.join("go/bin")),
            Some(PathAccess::ReadOnly)
        );
        assert_eq!(
            access_for(home_path.join("go/pkg/mod")),
            Some(PathAccess::ReadWrite)
        );
        assert_eq!(
            access_for(home_path.join(".npm/_cacache")),
            Some(PathAccess::ReadWrite)
        );
        assert_eq!(
            access_for(home_path.join(".bun/install/cache")),
            Some(PathAccess::ReadWrite)
        );

        #[cfg(target_os = "macos")]
        {
            assert_eq!(
                access_for(home_path.join("Library/Caches/go-build")),
                Some(PathAccess::ReadWrite)
            );
            assert_eq!(
                access_for(home_path.join("Library/Caches/uv")),
                Some(PathAccess::ReadWrite)
            );
            assert!(
                rules
                    .iter()
                    .all(|rule| rule.path() != home_path.join("Library/Caches")),
                "whole ~/Library/Caches must not be granted"
            );
        }

        // Credentials and Merry private directories must never be covered.
        for forbidden in [
            home_path.to_path_buf(),
            home_path.join(".cargo"),
            home_path.join(".cargo/credentials.toml"),
            home_path.join(".npmrc"),
            home_path.join(".pypirc"),
            home_path.join(".netrc"),
            home_path.join(".ssh"),
            home_path.join(".local/share"),
            home_path.join(".local/share/merry"),
            home_path.join("Library/Caches/com.apple.Safari"),
        ] {
            assert!(
                rules.iter().all(|rule| {
                    forbidden != rule.path() && !forbidden.starts_with(rule.path())
                }),
                "forbidden path {} must not be covered by baseline rules",
                forbidden.display()
            );
        }
    }
}
