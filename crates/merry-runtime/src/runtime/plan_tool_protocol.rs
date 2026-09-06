pub(super) struct PlanToolRejection {
    pub(super) code: &'static str,
    pub(super) message: String,
    pub(super) recovery: serde_json::Value,
}

impl PlanToolRejection {
    pub(super) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            recovery: read_plan_recovery(),
        }
    }

    pub(super) fn with_recovery(mut self, recovery: serde_json::Value) -> Self {
        self.recovery = recovery;
        self
    }
}

pub(super) fn no_active_plan_rejection() -> PlanToolRejection {
    PlanToolRejection::new("no_active_plan", "no active plan exists").with_recovery(
        serde_json::json!({
            "next_tool": "update_plan",
            "instruction": "Do not call read_plan again. If a durable plan is useful, use update_plan with expected_plan_revision 0 to define the first plan tree; the first valid update creates the active Plan. Otherwise continue with ordinary registered tools.",
            "example": {
                "reason": "Coordinate the requested multi-step work",
                "execution_intent": "continue_planning",
                "change": {
                    "type": "define_plan",
                    "expected_plan_revision": 0,
                    "root": {
                        "client_key": "root",
                        "objective": "Complete the requested work",
                        "acceptance": ["Focused checks pass"],
                        "depends_on": [],
                        "children": []
                    }
                }
            }
        }),
    )
}
pub(super) fn read_plan_recovery() -> serde_json::Value {
    serde_json::json!({
        "next_tool": "read_plan",
        "instruction": "Read the latest exact plan state before retrying a revision-sensitive operation."
    })
}
