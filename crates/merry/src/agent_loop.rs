use merry_runtime::{
    AgentLoopConfig, AgentLoopConfigError, DEFAULT_AGENT_LOOP_MAX_MODEL_TURNS,
    DEFAULT_CODING_AGENT_MAX_MODEL_TURNS,
};

/// Returns the default agent-loop config for generic embedded SDK usage.
///
/// This keeps the public facade aligned with runtime's default generic budget:
/// large enough for ordinary tool loops, but still bounded for application
/// calls that are not full coding-agent sessions.
pub fn generic_agent_loop_config() -> Result<AgentLoopConfig, AgentLoopConfigError> {
    AgentLoopConfig::new(DEFAULT_AGENT_LOOP_MAX_MODEL_TURNS)
}

/// Returns the default agent-loop config for coding-agent sessions.
///
/// Coding tasks need substantially more autonomous turns because a single user
/// request often includes exploration, tool retries, edits, and verification.
pub fn coding_agent_loop_config() -> Result<AgentLoopConfig, AgentLoopConfigError> {
    AgentLoopConfig::new(DEFAULT_CODING_AGENT_MAX_MODEL_TURNS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_loop_configs_match_runtime_constants() {
        assert_eq!(
            generic_agent_loop_config()
                .expect("generic config should be valid")
                .max_model_turns(),
            DEFAULT_AGENT_LOOP_MAX_MODEL_TURNS
        );
        assert_eq!(
            coding_agent_loop_config()
                .expect("coding config should be valid")
                .max_model_turns(),
            DEFAULT_CODING_AGENT_MAX_MODEL_TURNS
        );
    }
}
