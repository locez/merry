//! Cross-platform host path, directory, and executable-lookup helpers.
//!
//! Centralizes operating-system path conventions so lower and higher layers
//! agree on home/config/state/data/cache directories, Windows verbatim
//! (`\\?\`) path simplification, clean absolute path checks, and `PATH`
//! resolution with `PATHEXT`.

use std::{
    env,
    ffi::OsStr,
    fs, io,
    path::{Component, Path, PathBuf},
};

/// Default Windows executable extensions tried when `PATHEXT` is unset.
const DEFAULT_WINDOWS_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// Windows `MAX_PATH` limit where verbatim `\\?\` paths must be preserved.
const WINDOWS_MAX_PATH: usize = 260;

/// Resolves the current user's home directory from process environment.
///
/// On Unix, returns `HOME` when it is absolute. On Windows, prefers
/// `USERPROFILE`, then `HOMEDRIVE` + `HOMEPATH`, then `HOME` (for MSYS2 /
/// Git Bash and test overrides). Any Windows verbatim prefix is simplified
/// when safe.
#[must_use]
pub fn user_home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        user_home_dir_for_windows(
            env::var_os("USERPROFILE").as_deref(),
            env::var_os("HOMEDRIVE").as_deref(),
            env::var_os("HOMEPATH").as_deref(),
            env::var_os("HOME").as_deref(),
        )
    }
    #[cfg(not(windows))]
    {
        user_home_dir_for_unix(env::var_os("HOME").as_deref())
    }
}

/// Pure Unix home-directory resolution from an explicit `HOME` value.
#[cfg(not(windows))]
fn user_home_dir_for_unix(home: Option<&OsStr>) -> Option<PathBuf> {
    non_empty_absolute_path(home)
}

/// Pure Windows home-directory resolution from explicit environment values.
///
/// Prefers `USERPROFILE`, then `HOMEDRIVE` + `HOMEPATH`, then `HOME`.
#[cfg(any(windows, test))]
fn user_home_dir_for_windows(
    userprofile: Option<&OsStr>,
    homedrive: Option<&OsStr>,
    homepath: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    if let Some(path) = non_empty_windows_absolute_path(userprofile) {
        return Some(path);
    }
    if let (Some(drive), Some(path)) = (non_empty_str(homedrive), non_empty_str(homepath)) {
        let combined = format!("{drive}{path}");
        if is_windows_or_native_absolute(&combined) {
            return Some(PathBuf::from(simplify_windows_verbatim_owned(combined)));
        }
    }
    non_empty_windows_absolute_path(home)
}

/// Resolves the platform base directory for configuration files (`merry` is
/// appended by callers).
///
/// An explicit absolute `XDG_CONFIG_HOME` always wins. On Windows, falls back
/// to `%APPDATA%` when absolute, then `<home>/AppData/Roaming`. On Unix, falls
/// back to `<home>/.config`.
#[must_use]
pub fn platform_config_base(
    home: &Path,
    xdg_config_home: Option<&Path>,
    appdata: Option<&Path>,
) -> PathBuf {
    resolve_platform_base(
        cfg!(windows),
        home,
        xdg_config_home,
        appdata,
        &["AppData", "Roaming"],
        ".config",
    )
}

/// Resolves the platform base directory for runtime state and logs (`merry` is
/// appended by callers).
///
/// An explicit absolute `XDG_STATE_HOME` always wins. On Windows, falls back
/// to `%LOCALAPPDATA%` when absolute, then `<home>/AppData/Local`. On Unix,
/// falls back to `<home>/.local/state`.
#[must_use]
pub fn platform_state_base(
    home: &Path,
    xdg_state_home: Option<&Path>,
    local_appdata: Option<&Path>,
) -> PathBuf {
    resolve_platform_base(
        cfg!(windows),
        home,
        xdg_state_home,
        local_appdata,
        &["AppData", "Local"],
        ".local/state",
    )
}

/// Resolves the platform base directory for durable data (`merry` is appended
/// by callers).
///
/// An explicit absolute `XDG_DATA_HOME` always wins. On Windows, falls back to
/// `%LOCALAPPDATA%` when absolute, then `<home>/AppData/Local`. On Unix, falls
/// back to `<home>/.local/share`.
#[must_use]
pub fn platform_data_base(
    home: &Path,
    xdg_data_home: Option<&Path>,
    local_appdata: Option<&Path>,
) -> PathBuf {
    resolve_platform_base(
        cfg!(windows),
        home,
        xdg_data_home,
        local_appdata,
        &["AppData", "Local"],
        ".local/share",
    )
}

/// Resolves the platform base directory for caches and ephemeral worktrees
/// (`merry` is appended by callers).
///
/// An explicit absolute `XDG_CACHE_HOME` always wins. On Windows, falls back
/// to `%LOCALAPPDATA%` when absolute, then `<home>/AppData/Local`. On Unix,
/// falls back to `<home>/.cache`.
#[must_use]
pub fn platform_cache_base(
    home: &Path,
    xdg_cache_home: Option<&Path>,
    local_appdata: Option<&Path>,
) -> PathBuf {
    resolve_platform_base(
        cfg!(windows),
        home,
        xdg_cache_home,
        local_appdata,
        &["AppData", "Local"],
        ".cache",
    )
}

fn resolve_platform_base(
    windows: bool,
    home: &Path,
    xdg_override: Option<&Path>,
    windows_override: Option<&Path>,
    windows_fallback: &[&str],
    unix_fallback: &str,
) -> PathBuf {
    if let Some(path) = usable_base_path(windows, xdg_override) {
        return path;
    }
    if windows {
        if let Some(path) = usable_base_path(true, windows_override) {
            return path;
        }
        return windows_fallback
            .iter()
            .fold(home.to_path_buf(), |acc, segment| acc.join(segment));
    }
    home.join(unix_fallback)
}

/// Strips a leading `~/` or `~\` prefix from a user-configured path.
#[must_use]
pub fn strip_home_tilde_prefix(value: &str) -> Option<&str> {
    value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
}

/// Returns whether `path` is an absolute path without `.` or `..` components.
///
/// Accepts Windows drive/UNC prefixes (`Component::Prefix`) alongside
/// `Component::RootDir` and `Component::Normal`.
#[must_use]
pub fn is_clean_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path.components().all(|component| {
            matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            )
        })
}

/// Canonicalizes `path` and simplifies a Windows verbatim (`\\?\`) prefix when
/// safe so paths remain comparable with user/model inputs and external tools
/// such as `git`.
pub fn canonicalize_clean(path: &Path) -> io::Result<PathBuf> {
    let canonical = fs::canonicalize(path)?;
    Ok(simplify_verbatim_path(&canonical))
}

/// Simplifies a Windows verbatim (`\\?\`) path prefix when safe.
///
/// Leaves paths untouched on non-Windows targets or when the simplified form
/// would violate Windows `MAX_PATH`, reserved DOS device name, or trailing
/// dot/space rules.
#[must_use]
pub fn simplify_verbatim_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        match path.to_str().and_then(simplify_windows_verbatim_str) {
            Some(simplified) => PathBuf::from(simplified),
            None => path.to_path_buf(),
        }
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

/// Pure Windows verbatim-prefix simplifier (`\\?\C:\...` -> `C:\...`,
/// `\\?\UNC\server\share\...` -> `\\server\share\...`).
///
/// Returns `None` if `raw` does not carry a simplifiable verbatim prefix or
/// must retain verbatim syntax for Windows filesystem safety.
fn simplify_windows_verbatim_str(raw: &str) -> Option<String> {
    let stripped = if let Some(unc_rest) = raw
        .strip_prefix(r"\\?\UNC\")
        .or_else(|| raw.strip_prefix(r"\\?\unc\"))
    {
        format!(r"\\{unc_rest}")
    } else {
        let rest = raw.strip_prefix(r"\\?\")?;
        let bytes = rest.as_bytes();
        if bytes.len() < 3
            || !bytes[0].is_ascii_alphabetic()
            || bytes[1] != b':'
            || bytes[2] != b'\\'
        {
            return None;
        }
        rest.to_owned()
    };

    if stripped.len() >= WINDOWS_MAX_PATH {
        return None;
    }
    if stripped.contains('/') {
        return None;
    }
    for segment in stripped.split('\\').skip(1) {
        if segment.is_empty() {
            continue;
        }
        if segment == "." || segment == ".." {
            return None;
        }
        if segment.ends_with('.') || segment.ends_with(' ') {
            return None;
        }
        if is_windows_reserved_device_name(segment) {
            return None;
        }
    }
    Some(stripped)
}

/// Searches `path_env` for the first runnable executable matching any entry in
/// `names`, expanding `PATHEXT` extensions on Windows.
pub fn find_executable_in_path(
    path_env: &OsStr,
    names: &[&str],
    mut is_executable: impl FnMut(&Path) -> bool,
) -> Option<PathBuf> {
    let pathext = env::var_os("PATHEXT");
    find_executable_in_path_for(
        path_env,
        names,
        pathext.as_deref(),
        cfg!(windows),
        &mut is_executable,
    )
}

/// Pure testable variant of [`find_executable_in_path`].
pub fn find_executable_in_path_for(
    path_env: &OsStr,
    names: &[&str],
    pathext: Option<&OsStr>,
    use_pathext: bool,
    mut is_executable: impl FnMut(&Path) -> bool,
) -> Option<PathBuf> {
    let extensions = if use_pathext {
        parse_pathext(pathext)
    } else {
        Vec::new()
    };

    for directory in env::split_paths(path_env) {
        if directory.as_os_str().is_empty() {
            continue;
        }
        for name in names {
            let direct = directory.join(name);
            if is_executable(&direct) {
                return Some(direct);
            }
            if use_pathext && !has_windows_executable_extension(name, &extensions) {
                for ext in &extensions {
                    let candidate = directory.join(format!("{name}{ext}"));
                    if is_executable(&candidate) {
                        return Some(candidate);
                    }
                }
            }
        }
    }
    None
}

fn parse_pathext(pathext: Option<&OsStr>) -> Vec<String> {
    let raw = pathext
        .and_then(OsStr::to_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(DEFAULT_WINDOWS_PATHEXT);
    let mut extensions = Vec::new();
    for entry in raw.split(';') {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = if trimmed.starts_with('.') {
            trimmed.to_ascii_lowercase()
        } else {
            format!(".{}", trimmed.to_ascii_lowercase())
        };
        if !extensions.contains(&normalized) {
            extensions.push(normalized);
        }
    }
    if extensions.is_empty() {
        extensions.push(".exe".to_owned());
    }
    extensions
}

fn has_windows_executable_extension(name: &str, extensions: &[String]) -> bool {
    let lower = name.to_ascii_lowercase();
    extensions.iter().any(|ext| lower.ends_with(ext))
}

fn is_windows_reserved_device_name(segment: &str) -> bool {
    let stem = segment.split('.').next().unwrap_or(segment);
    let upper = stem.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

#[cfg(any(windows, test))]
fn non_empty_str(value: Option<&OsStr>) -> Option<&str> {
    value.and_then(OsStr::to_str).filter(|s| !s.is_empty())
}

#[cfg(not(windows))]
fn non_empty_absolute_path(value: Option<&OsStr>) -> Option<PathBuf> {
    value
        .filter(|raw| !raw.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

#[cfg(any(windows, test))]
fn non_empty_windows_absolute_path(value: Option<&OsStr>) -> Option<PathBuf> {
    let raw = value.filter(|raw| !raw.is_empty())?;
    let as_str = raw.to_str();
    if let Some(text) = as_str {
        if is_windows_or_native_absolute(text) {
            return Some(PathBuf::from(simplify_windows_verbatim_owned(
                text.to_owned(),
            )));
        }
        return None;
    }
    let path = PathBuf::from(raw);
    path.is_absolute().then_some(path)
}

fn is_windows_or_native_absolute(value: &str) -> bool {
    if Path::new(value).is_absolute() {
        return true;
    }
    let bytes = value.as_bytes();
    (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/'))
        || value.starts_with(r"\\")
}

fn usable_base_path(windows: bool, candidate: Option<&Path>) -> Option<PathBuf> {
    let path = candidate?;
    if path.as_os_str().is_empty() {
        return None;
    }
    let is_abs = if windows {
        path.to_str().is_some_and(is_windows_or_native_absolute) || path.is_absolute()
    } else {
        path.is_absolute()
    };
    if !is_abs {
        return None;
    }
    Some(if windows {
        match path.to_str().and_then(simplify_windows_verbatim_str) {
            Some(simplified) => PathBuf::from(simplified),
            None => path.to_path_buf(),
        }
    } else {
        path.to_path_buf()
    })
}

#[cfg(any(windows, test))]
fn simplify_windows_verbatim_owned(raw: String) -> String {
    simplify_windows_verbatim_str(&raw).unwrap_or(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn windows_home_prefers_userprofile_then_homedrive_homepath_then_home() {
        assert_eq!(
            user_home_dir_for_windows(
                Some(OsStr::new(r"C:\Users\Alice")),
                Some(OsStr::new("D:")),
                Some(OsStr::new(r"\Users\Bob")),
                Some(OsStr::new(r"E:\Home")),
            ),
            Some(PathBuf::from(r"C:\Users\Alice"))
        );
        assert_eq!(
            user_home_dir_for_windows(
                None,
                Some(OsStr::new("C:")),
                Some(OsStr::new(r"\Users\Bob")),
                Some(OsStr::new(r"E:\Home")),
            ),
            Some(PathBuf::from(r"C:\Users\Bob"))
        );
        assert_eq!(
            user_home_dir_for_windows(None, None, None, Some(OsStr::new(r"C:\Users\Fallback"))),
            Some(PathBuf::from(r"C:\Users\Fallback"))
        );
        assert_eq!(
            user_home_dir_for_windows(Some(OsStr::new("relative")), None, None, None),
            None
        );
    }

    #[test]
    fn windows_platform_bases_respect_xdg_overrides_and_fall_back_to_appdata() {
        let home = Path::new(r"C:\Users\Alice");
        assert_eq!(
            resolve_platform_base(
                true,
                home,
                None,
                Some(Path::new(r"C:\Users\Alice\AppData\Roaming")),
                &["AppData", "Roaming"],
                ".config",
            ),
            PathBuf::from(r"C:\Users\Alice\AppData\Roaming")
        );
        assert_eq!(
            resolve_platform_base(
                true,
                home,
                Some(Path::new(r"D:\CustomConfig")),
                Some(Path::new(r"C:\Users\Alice\AppData\Roaming")),
                &["AppData", "Roaming"],
                ".config",
            ),
            PathBuf::from(r"D:\CustomConfig")
        );
        assert_eq!(
            resolve_platform_base(
                true,
                home,
                None,
                Some(Path::new(r"C:\Users\Alice\AppData\Local")),
                &["AppData", "Local"],
                ".local/state",
            ),
            PathBuf::from(r"C:\Users\Alice\AppData\Local")
        );
        assert_eq!(
            resolve_platform_base(
                true,
                home,
                None,
                None,
                &["AppData", "Local"],
                ".local/share"
            ),
            home.join("AppData").join("Local")
        );
    }

    #[test]
    fn simplifies_safe_windows_verbatim_paths_and_preserves_unsafe_ones() {
        assert_eq!(
            simplify_windows_verbatim_str(r"\\?\C:\Users\Alice\repo"),
            Some(r"C:\Users\Alice\repo".to_owned())
        );
        assert_eq!(
            simplify_windows_verbatim_str(r"\\?\UNC\server\share\repo"),
            Some(r"\\server\share\repo".to_owned())
        );
        assert_eq!(simplify_windows_verbatim_str(r"C:\Users\Alice"), None);
        assert_eq!(
            simplify_windows_verbatim_str(r"\\?\C:\Users\Alice\NUL\file.txt"),
            None
        );
        assert_eq!(
            simplify_windows_verbatim_str(r"\\?\C:\Users\Alice\trailing."),
            None
        );
        let long = format!(r"\\?\C:\{}", "a".repeat(260));
        assert_eq!(simplify_windows_verbatim_str(&long), None);
    }

    #[test]
    fn strip_home_tilde_prefix_accepts_forward_and_backslash() {
        assert_eq!(
            strip_home_tilde_prefix("~/.config/merry"),
            Some(".config/merry")
        );
        assert_eq!(
            strip_home_tilde_prefix(r"~\AppData\Roaming"),
            Some(r"AppData\Roaming")
        );
        assert_eq!(strip_home_tilde_prefix("~alice/foo"), None);
    }

    #[test]
    fn find_executable_in_path_for_expands_pathext_when_enabled() {
        let bin_dir = Path::new("/tools/bin");
        let expected = bin_dir.join("rg.exe");
        let path_env = env::join_paths([bin_dir]).expect("join paths");
        let found = find_executable_in_path_for(
            &path_env,
            &["rg"],
            Some(&OsString::from(".COM;.EXE;.CMD")),
            true,
            |candidate| candidate == expected,
        );
        assert_eq!(found, Some(expected.clone()));

        let not_found_unix = find_executable_in_path_for(
            &path_env,
            &["rg"],
            Some(&OsString::from(".EXE")),
            false,
            |candidate| candidate == expected,
        );
        assert_eq!(not_found_unix, None);
    }
}
