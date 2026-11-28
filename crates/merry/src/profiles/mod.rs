//! Runtime profile components.

pub use crate::coding::surface::*;

pub use merry_process::{ProcessBackend, ProcessSession};

pub use merry_runtime::{
    AcceptedLocalWorkspaceProcessAdmission, PathAccess, PathAccessRule, PathAccessRuleSource,
    RuntimeCapabilities, RuntimeProfile, RuntimeProfileBuilder, RuntimeProfileError,
};

#[cfg(test)]
mod tests {
    use super::coding_agent;

    #[test]
    fn facade_uses_the_shared_coding_agent_profile() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let profile = coding_agent(temp.path())
            .build()
            .expect("shared coding profile should build");

        assert_eq!(
            profile
                .tool_names()
                .into_iter()
                .map(|name| name.as_str())
                .collect::<Vec<_>>(),
            ["read_file"]
        );
        assert!(profile.profile_hash().as_str().starts_with("fnv1a64:"));
    }
}
