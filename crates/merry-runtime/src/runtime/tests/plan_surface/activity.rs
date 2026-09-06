use crate::runtime::{Runtime, tests::plan_surface::session_id};
use merry_core::{SubagentActivityPhase, SubagentActivitySnapshot, SubagentId, SubagentTaskId};
use std::sync::Arc;

#[tokio::test(flavor = "current_thread")]
async fn runtime_subagent_activity_subscription_reuses_builder_hub() {
    let hub = Arc::new(crate::SubagentActivityHub::new());
    let runtime = Runtime::builder(session_id("activity-subscription"))
        .subagent_activity_hub(Arc::clone(&hub))
        .build()
        .expect("runtime builds");
    let mut receiver = runtime.subscribe_subagent_activity();
    assert!(receiver.borrow().is_empty());

    hub.publish(SubagentActivitySnapshot {
        subagent_id: SubagentId::new("activity-agent").expect("valid agent id"),
        task_id: SubagentTaskId::new("activity-task").expect("valid task id"),
        phase: SubagentActivityPhase::Starting,
        summary: "starting".to_owned(),
        updated_at_ms: 1,
    });
    assert!(
        receiver
            .has_changed()
            .expect("activity receiver remains open")
    );
    assert_eq!(
        receiver.borrow_and_update()[0].phase,
        SubagentActivityPhase::Starting
    );
}

#[tokio::test(flavor = "current_thread")]
async fn activity_projection_does_not_change_plan_revision_or_emit_plan_event() {
    let hub = Arc::new(crate::SubagentActivityHub::new());
    let runtime = Runtime::builder(session_id("activity-plan-isolation"))
        .coordinator_plan_tools()
        .subagent_activity_hub(Arc::clone(&hub))
        .build()
        .expect("runtime builds");
    runtime
        .begin_plan(crate::plan::BeginPlanInput {
            reason: "establish an activity isolation plan".to_owned(),
            governing_skill_id: None,
        })
        .await
        .expect("plan activates");
    let before = runtime
        .plan_snapshot()
        .await
        .expect("plan snapshot reads")
        .expect("active plan exists");
    let mut plan_events = runtime.subscribe_plan_events();

    hub.publish(SubagentActivitySnapshot {
        subagent_id: SubagentId::new("isolated-agent").expect("valid agent id"),
        task_id: SubagentTaskId::new("isolated-task").expect("valid task id"),
        phase: SubagentActivityPhase::Running,
        summary: "working".to_owned(),
        updated_at_ms: 1,
    });

    let after = runtime
        .plan_snapshot()
        .await
        .expect("plan snapshot reads")
        .expect("active plan exists");
    assert_eq!(after.revision, before.revision);
    assert!(plan_events.try_recv().is_err());
}
