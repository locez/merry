use crate::{
    StructuredAnswer, TestProfile, bridge_tool_named, model_name, session_id, text_provider,
};
use merry::{AgentBuilder, FileSessionStore};

#[test]
fn builder_accepts_a_generic_agent_profile() {
    let agent = AgentBuilder::new(session_id("generic-profile"))
        .model_provider(text_provider("profile"), model_name())
        .profile(TestProfile)
        .expect("generic profile should apply")
        .build()
        .expect("agent should build");

    assert_eq!(agent.loop_config().max_model_turns(), 2);
    assert!(agent.profile().is_some());
}

#[test]
fn coding_profile_is_accepted_through_the_generic_profile_boundary() {
    let root = tempfile::tempdir().expect("profile workspace should be created");
    let profile = merry::profiles::coding_agent(root.path())
        .build()
        .expect("coding profile should build");

    let agent = AgentBuilder::new(session_id("coding-profile"))
        .model_provider(text_provider("profile"), model_name())
        .profile(profile)
        .expect("coding profile should apply")
        .build()
        .expect("agent should build");

    assert!(agent.profile().is_some());
}

#[tokio::test]
async fn builder_resumes_a_saved_session_with_the_same_contract() {
    let temp = tempfile::tempdir().expect("session store directory should be created");
    let store = FileSessionStore::new(temp.path());
    let session = session_id("resume-test");
    let first_agent = AgentBuilder::new(session.clone())
        .model_provider(text_provider("saved"), model_name())
        .session_store(store.clone())
        .build()
        .expect("first agent should build");

    first_agent
        .run("create persisted state")
        .await
        .expect("first run should complete");
    first_agent
        .save_session()
        .await
        .expect("session should be saved");

    let resumed = AgentBuilder::new(session)
        .model_provider(text_provider("resumed"), model_name())
        .resume_from_store(store)
        .await
        .expect("saved session should resume");

    assert_eq!(resumed.session_id().as_str(), "resume-test");
    assert!(resumed.run("continue").await.is_ok());
}

#[test]
fn building_without_a_primary_provider_is_rejected() {
    let error = match AgentBuilder::new(session_id("missing-provider")).build() {
        Ok(_) => panic!("provider-less agent should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.to_string(), "a primary model provider is required");
}

#[test]
fn typed_tool_rejects_the_runtime_final_output_name() {
    let error = merry::Tool::new(
        merry_runtime::FINAL_OUTPUT_TOOL_NAME,
        "Application tool using a reserved name.",
        |_input: StructuredAnswer| async { Ok::<String, String>(String::new()) },
    )
    .expect_err("runtime-owned final output name must be rejected");

    assert!(matches!(
        error,
        merry::ToolBuildError::ReservedName { name }
            if name.as_str() == merry_runtime::FINAL_OUTPUT_TOOL_NAME
    ));
}

#[test]
fn runtime_rejects_a_reserved_final_output_tool_name() {
    let result = merry_runtime::Runtime::builder(session_id("reserved-final-output-tool"))
        .register_tool(bridge_tool_named(merry_runtime::FINAL_OUTPUT_TOOL_NAME))
        .build();

    assert!(matches!(
        result,
        Err(merry_runtime::RuntimeError::ReservedToolName { name })
            if name.as_str() == merry_runtime::FINAL_OUTPUT_TOOL_NAME
    ));
}
