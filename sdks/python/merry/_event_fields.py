"""Strict field contracts for normalized runtime events."""

from ._event_types import EventType


def _event_fields(
    *fields: str, optional: tuple[str, ...] = ()
) -> tuple[frozenset[str], frozenset[str]]:
    return frozenset(("type", *fields)), frozenset(optional)


EVENT_FIELDS: dict[EventType, tuple[frozenset[str], frozenset[str]]] = {
    EventType.MODEL_OUTPUT_RATE_UPDATED: _event_fields("rate", "source"),
    EventType.SESSION_STARTED: _event_fields("source"),
    EventType.STEP_STARTED: _event_fields("source"),
    EventType.STEP_COMPLETED: _event_fields("source"),
    EventType.COMPACTION_STARTED: _event_fields("source"),
    EventType.COMPACTION_COMPLETED: _event_fields(
        "checkpoint_id", "covered_history_item_count", "source"
    ),
    EventType.USAGE_UPDATED: _event_fields("usage", "source"),
    EventType.ASSISTANT_MESSAGE: _event_fields("text", "source"),
    EventType.ASSISTANT_MESSAGE_DELTA: _event_fields("delta", "source"),
    EventType.REASONING_DELTA: _event_fields("delta", "source"),
    EventType.TOOL_CALL_STARTED: _event_fields("call", "source"),
    EventType.TOOL_CALL_BATCH_STARTED: _event_fields("batch", "source"),
    EventType.TOOL_CALL_FINISHED: _event_fields("result", "output", "source"),
    EventType.FINAL_OUTPUT_RECORDED: _event_fields("call_id", "json", "source"),
    EventType.MODEL_RETRY_ATTEMPT_STARTED: _event_fields(
        "attempt", "max_attempts", "source"
    ),
    EventType.MODEL_RETRY_SCHEDULED: _event_fields(
        "attempt", "next_attempt", "max_attempts", "delay_ms", "error_kind", "source"
    ),
    EventType.MODEL_RETRY_EXHAUSTED: _event_fields(
        "attempts_run", "max_attempts", "error_kind", "source"
    ),
    EventType.SKILL_USED: _event_fields(
        "skill_name", "skill_md_path", "tool_call_id", "source"
    ),
    EventType.SUBAGENT_SPAWNED: _event_fields(
        "agent_id",
        "task_id",
        "task_anchor",
        "source",
        optional=("display_name", "role"),
    ),
    EventType.SUBAGENT_STARTED: _event_fields("agent_id", "task_id", "source"),
    EventType.SUBAGENT_STATUS_CHANGED: _event_fields(
        "agent_id", "task_id", "status", "source"
    ),
    EventType.SUBAGENT_COMPLETED: _event_fields(
        "agent_id",
        "task_id",
        "summary",
        "output_paths",
        "changed_paths",
        "source",
        optional=("usage",),
    ),
    EventType.SUBAGENT_FAILED: _event_fields(
        "agent_id", "task_id", "diagnostic", "source"
    ),
    EventType.SUBAGENT_CANCELLED: _event_fields(
        "agent_id", "task_id", "diagnostic", "source"
    ),
    EventType.PLAN_MODE_CHANGED: _event_fields("mode", "source"),
    EventType.PLAN_SAVED: _event_fields("plan", "source"),
    EventType.PLAN_APPROVAL_REQUESTED: _event_fields("plan", "source"),
    EventType.PLAN_REVIEWED: _event_fields("plan_id", "decision", "source"),
    EventType.TODO_LIST_UPDATED: _event_fields("items", "source"),
    EventType.RUN_FAILED: _event_fields("diagnostic", "source"),
    EventType.RUN_CANCELLED: _event_fields("diagnostic", "source"),
    EventType.INTERACTIVE_RUN_STATE_CHANGED: _event_fields("state"),
    EventType.QUEUED_INPUT_ACCEPTED: _event_fields("inputs"),
    EventType.QUEUED_INPUTS_CHANGED: _event_fields("inputs"),
    EventType.CLOSED: _event_fields(),
}
