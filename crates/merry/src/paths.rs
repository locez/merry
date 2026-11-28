//! Cross-platform host path conventions and shared protected-path contracts.

pub use merry_core::host_paths::{
    ProtectedHostPaths, canonicalize_clean, find_executable_in_path, find_executable_in_path_for,
    is_clean_absolute_path, platform_cache_base, platform_config_base, platform_data_base,
    platform_state_base, simplify_verbatim_path, strip_home_tilde_prefix, user_home_dir,
};
