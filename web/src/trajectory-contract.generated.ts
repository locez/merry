// This file is generated from merry-core's TrajectoryEvent JSON Schema.
// Do not edit it directly; run npm run generate:trajectory-contract.

export type JsonValue = null | boolean | number | string | JsonValue[] | JsonObject;
export type JsonObject = { readonly [key: string]: JsonValue };
export type JsonSchemaValue = boolean | JsonObject;
export type Schema = JsonSchemaValue;
export type WireInteger = bigint;

export type ArtifactId = string;

export type ArtifactKind = "text" | "json" | "binary" | "image" | "other";
export const ARTIFACT_KIND_VALUES = ["text","json","binary","image","other"] as const;

export type ArtifactRef = {
  readonly "id": ArtifactId;
  readonly "kind": ArtifactKind;
  readonly "label": string | null;
};
export const ARTIFACT_REF_FIELDS = ["id","kind","label"] as const;

export type ErrorInfo = {
  readonly "code": string;
  readonly "message": string;
};
export const ERROR_INFO_FIELDS = ["code","message"] as const;

export type SessionId = string;

export type ToolCallArguments = JsonObject;

export type ToolCallId = string;

export type ToolInputSchema = JsonSchemaValue;

export type ToolName = string;

export type ToolSpec = {
  readonly "description": string;
  readonly "input_schema": ToolInputSchema;
  readonly "name": ToolName;
};
export const TOOL_SPEC_FIELDS = ["description","input_schema","name"] as const;

export type TrajectoryLane = "input" | "model" | "tools" | "system";
export const TRAJECTORY_LANE_VALUES = ["input","model","tools","system"] as const;

export type TrajectoryPayload = {
  readonly "content": string;
  readonly "kind": TrajectoryPayloadKind;
  readonly "truncated": boolean;
};
export const TRAJECTORY_PAYLOAD_FIELDS = ["content","kind","truncated"] as const;

export type TrajectoryPayloadKind = "text" | "json";
export const TRAJECTORY_PAYLOAD_KIND_VALUES = ["text","json"] as const;

export type TrajectoryPromptBlock = {
  readonly "content": string;
  readonly "id": TrajectoryRecordId;
  readonly "sequence_order": number;
  readonly "truncated": boolean;
};
export const TRAJECTORY_PROMPT_BLOCK_FIELDS = ["content","id","sequence_order","truncated"] as const;

export type TrajectoryPromptSnapshot = {
  readonly "dynamic_context_count": WireInteger;
  readonly "latest_dynamic_sequence": WireInteger | null;
  readonly "stable_blocks": readonly TrajectoryPromptBlock[];
};
export const TRAJECTORY_PROMPT_SNAPSHOT_FIELDS = ["dynamic_context_count","latest_dynamic_sequence","stable_blocks"] as const;

export type TrajectoryRecord = {
  readonly "artifacts": readonly ArtifactRef[];
  readonly "details": TrajectoryRecordDetails;
  readonly "diagnostic": ErrorInfo | null;
  readonly "end_sequence": WireInteger | null;
  readonly "finished_at_ms": WireInteger | null;
  readonly "id": TrajectoryRecordId;
  readonly "kind": TrajectoryRecordKind;
  readonly "label": string;
  readonly "lane": TrajectoryLane;
  readonly "parent_id": TrajectoryRecordId | null;
  readonly "sequence_order": number;
  readonly "start_sequence": WireInteger;
  readonly "started_at_ms": WireInteger | null;
  readonly "status": TrajectoryRecordStatus;
  readonly "summary": string | null;
  readonly "tool_call_id": ToolCallId | null;
  readonly "turn_id": WireInteger | null;
};
export const TRAJECTORY_RECORD_FIELDS = ["artifacts","details","diagnostic","end_sequence","finished_at_ms","id","kind","label","lane","parent_id","sequence_order","start_sequence","started_at_ms","status","summary","tool_call_id","turn_id"] as const;

export type TrajectoryRecordDetails = {
  readonly "type": "none";
} | {
  readonly "content": string;
  readonly "truncated": boolean;
  readonly "type": "message";
} | {
  readonly "tool": TrajectoryToolDetails;
  readonly "type": "tool";
};
export const TRAJECTORY_RECORD_DETAILS_NONE_FIELDS = ["type"] as const;
export const TRAJECTORY_RECORD_DETAILS_MESSAGE_FIELDS = ["content","truncated","type"] as const;
export const TRAJECTORY_RECORD_DETAILS_TOOL_FIELDS = ["tool","type"] as const;

export type TrajectoryRecordId = string;

export type TrajectoryRecordKind = "user_input" | "assistant_message" | "tool_call" | "tool_result" | "compaction" | "lifecycle";
export const TRAJECTORY_RECORD_KIND_VALUES = ["user_input","assistant_message","tool_call","tool_result","compaction","lifecycle"] as const;

export type TrajectoryRecordStatus = "pending" | "running" | "succeeded" | "failed" | "cancelled" | "completed";
export const TRAJECTORY_RECORD_STATUS_VALUES = ["pending","running","succeeded","failed","cancelled","completed"] as const;

export type TrajectorySnapshot = {
  readonly "closed": boolean;
  readonly "history_truncated_before": WireInteger | null;
  readonly "latest_sequence": WireInteger;
  readonly "prompt": TrajectoryPromptSnapshot;
  readonly "records": readonly TrajectoryRecord[];
  readonly "revision": WireInteger;
  readonly "session_id": SessionId;
  readonly "tool_specs": readonly ToolSpec[];
};
export const TRAJECTORY_SNAPSHOT_FIELDS = ["closed","history_truncated_before","latest_sequence","prompt","records","revision","session_id","tool_specs"] as const;

export type TrajectoryToolDetails = {
  readonly "arguments": JsonObject;
  readonly "arguments_json": string;
  readonly "output": TrajectoryPayload | null;
  readonly "tool_name": ToolName | null;
};
export const TRAJECTORY_TOOL_DETAILS_FIELDS = ["arguments","arguments_json","output","tool_name"] as const;

export const TRAJECTORY_EVENT_SNAPSHOT_FIELDS = ["snapshot","type"] as const;
export const TRAJECTORY_EVENT_RECORD_UPSERT_FIELDS = ["latest_sequence","record","revision","type"] as const;
export const TRAJECTORY_EVENT_PROMPT_UPDATED_FIELDS = ["latest_sequence","prompt","revision","type"] as const;
export const TRAJECTORY_EVENT_SESSION_CLOSED_FIELDS = ["latest_sequence","revision","type"] as const;

export type TrajectoryEvent = {
  readonly "snapshot": TrajectorySnapshot;
  readonly "type": "snapshot";
} | {
  readonly "latest_sequence": WireInteger;
  readonly "record": TrajectoryRecord;
  readonly "revision": WireInteger;
  readonly "type": "record_upsert";
} | {
  readonly "latest_sequence": WireInteger;
  readonly "prompt": TrajectoryPromptSnapshot;
  readonly "revision": WireInteger;
  readonly "type": "prompt_updated";
} | {
  readonly "latest_sequence": WireInteger;
  readonly "revision": WireInteger;
  readonly "type": "session_closed";
};
export type ArtifactReference = ArtifactRef;
export type Diagnostic = ErrorInfo;
