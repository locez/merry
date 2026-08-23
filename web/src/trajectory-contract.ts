export type * from "./trajectory-contract.generated.js";
import type {
  ArtifactReference,
  Diagnostic,
  JsonObject,
  JsonSchemaValue,
  JsonValue,
  ToolSpec,
  TrajectoryEvent,
  TrajectoryLane,
  TrajectoryPayload,
  TrajectoryPromptBlock,
  TrajectoryPromptSnapshot,
  TrajectoryRecord,
  TrajectoryRecordDetails,
  TrajectoryRecordKind,
  TrajectoryRecordStatus,
  TrajectorySnapshot,
  WireInteger,
} from "./trajectory-contract.generated.js";
import {
  ARTIFACT_REF_FIELDS,
  ARTIFACT_KIND_VALUES,
  ERROR_INFO_FIELDS,
  TOOL_SPEC_FIELDS,
  TRAJECTORY_EVENT_PROMPT_UPDATED_FIELDS,
  TRAJECTORY_EVENT_RECORD_UPSERT_FIELDS,
  TRAJECTORY_EVENT_SESSION_CLOSED_FIELDS,
  TRAJECTORY_EVENT_SNAPSHOT_FIELDS,
  TRAJECTORY_LANE_VALUES,
  TRAJECTORY_PAYLOAD_FIELDS,
  TRAJECTORY_PAYLOAD_KIND_VALUES,
  TRAJECTORY_PROMPT_BLOCK_FIELDS,
  TRAJECTORY_PROMPT_SNAPSHOT_FIELDS,
  TRAJECTORY_RECORD_DETAILS_MESSAGE_FIELDS,
  TRAJECTORY_RECORD_DETAILS_NONE_FIELDS,
  TRAJECTORY_RECORD_DETAILS_TOOL_FIELDS,
  TRAJECTORY_RECORD_FIELDS,
  TRAJECTORY_RECORD_KIND_VALUES,
  TRAJECTORY_RECORD_STATUS_VALUES,
  TRAJECTORY_SNAPSHOT_FIELDS,
  TRAJECTORY_TOOL_DETAILS_FIELDS,
} from "./trajectory-contract.generated.js";

const laneChoices: readonly TrajectoryLane[] = TRAJECTORY_LANE_VALUES;
const kindChoices: readonly TrajectoryRecordKind[] = TRAJECTORY_RECORD_KIND_VALUES;
const statusChoices: readonly TrajectoryRecordStatus[] = TRAJECTORY_RECORD_STATUS_VALUES;

export function decodeSnapshot(value: unknown): TrajectorySnapshot {
  const object = readObject(value, "trajectory snapshot", TRAJECTORY_SNAPSHOT_FIELDS);
  return {
    session_id: readString(object.session_id, "session_id"),
    revision: readU64(object.revision, "revision"),
    latest_sequence: readU64(object.latest_sequence, "latest_sequence"),
    closed: object.closed === undefined ? false : readBoolean(object.closed, "closed"),
    history_truncated_before: readNullableU64(
      object.history_truncated_before,
      "history_truncated_before",
    ),
    prompt: decodePrompt(object.prompt),
    tool_specs: readArray(object.tool_specs, "tool_specs").map(decodeToolSpec),
    records: readArray(object.records, "records").map(decodeRecord),
  };
}

export function decodeEvent(value: unknown): TrajectoryEvent {
  const object = readObject(value, "trajectory event");
  const type = readString(object.type, "event.type");
  if (type === "snapshot") {
    assertKeys(object, TRAJECTORY_EVENT_SNAPSHOT_FIELDS, "snapshot event");
    return { type, snapshot: decodeSnapshot(object.snapshot) };
  }
  if (type === "record_upsert") {
    assertKeys(object, TRAJECTORY_EVENT_RECORD_UPSERT_FIELDS, "record event");
    return {
      type,
      revision: readU64(object.revision, "revision"),
      latest_sequence: readU64(object.latest_sequence, "latest_sequence"),
      record: decodeRecord(object.record),
    };
  }
  if (type === "session_closed") {
    assertKeys(object, TRAJECTORY_EVENT_SESSION_CLOSED_FIELDS, "session closed event");
    return {
      type,
      revision: readU64(object.revision, "revision"),
      latest_sequence: readU64(object.latest_sequence, "latest_sequence"),
    };
  }
  if (type === "prompt_updated") {
    assertKeys(object, TRAJECTORY_EVENT_PROMPT_UPDATED_FIELDS, "prompt event");
    return {
      type,
      revision: readU64(object.revision, "revision"),
      latest_sequence: readU64(object.latest_sequence, "latest_sequence"),
      prompt: decodePrompt(object.prompt),
    };
  }
  throw new Error(`unsupported trajectory event: ${type}`);
}

function decodeRecord(value: unknown): TrajectoryRecord {
  const object = readObject(value, "trajectory record", TRAJECTORY_RECORD_FIELDS);
  return {
    id: readString(object.id, "record.id"),
    lane: readEnum(object.lane, laneChoices, "record.lane"),
    kind: readEnum(object.kind, kindChoices, "record.kind"),
    label: readString(object.label, "record.label"),
    summary: readNullableString(object.summary, "record.summary"),
    status: readEnum(object.status, statusChoices, "record.status"),
    start_sequence: readU64(object.start_sequence, "record.start_sequence"),
    sequence_order: readNumber(object.sequence_order, "record.sequence_order"),
    turn_id: readNullableU64(object.turn_id, "record.turn_id"),
    end_sequence: readNullableU64(object.end_sequence, "record.end_sequence"),
    parent_id: readNullableString(object.parent_id, "record.parent_id"),
    tool_call_id: readNullableString(object.tool_call_id, "record.tool_call_id"),
    artifacts: readArray(object.artifacts, "record.artifacts").map(decodeArtifact),
    diagnostic: object.diagnostic === undefined || object.diagnostic === null
      ? null
      : decodeDiagnostic(object.diagnostic),
    started_at_ms: readNullableU64(object.started_at_ms, "record.started_at_ms"),
    finished_at_ms: readNullableU64(object.finished_at_ms, "record.finished_at_ms"),
    details: decodeDetails(object.details),
  };
}

function decodeDetails(value: unknown): TrajectoryRecordDetails {
  const object = readObject(value, "record.details");
  const type = readString(object.type, "record.details.type");
  if (type === "none") {
    assertKeys(object, TRAJECTORY_RECORD_DETAILS_NONE_FIELDS, "record.details.none");
    return { type };
  }
  if (type === "message") {
    assertKeys(object, TRAJECTORY_RECORD_DETAILS_MESSAGE_FIELDS, "record.details.message");
    return {
      type,
      content: readString(object.content, "record.details.content"),
      truncated: readBoolean(object.truncated, "record.details.truncated"),
    };
  }
  if (type === "tool") {
    assertKeys(object, TRAJECTORY_RECORD_DETAILS_TOOL_FIELDS, "record.details.tool");
    const tool = readObject(object.tool, "record.details.tool.value", TRAJECTORY_TOOL_DETAILS_FIELDS);
    return {
      type,
      tool: {
        tool_name: readNullableToolName(tool.tool_name, "record.details.tool.tool_name"),
        arguments: readJsonObject(tool.arguments, "record.details.arguments"),
        arguments_json: tool.arguments_json === undefined
          ? ""
          : readString(tool.arguments_json, "record.details.arguments_json"),
        output: tool.output === undefined || tool.output === null ? null : decodePayload(tool.output),
      },
    };
  }
  throw new Error(`record.details.type has an unsupported value: ${type}`);
}

function decodePrompt(value: unknown): TrajectoryPromptSnapshot {
  const object = readObject(value, "trajectory prompt", TRAJECTORY_PROMPT_SNAPSHOT_FIELDS);
  return {
    stable_blocks: readArray(object.stable_blocks, "prompt.stable_blocks").map(decodePromptBlock),
    dynamic_context_count: readU64(object.dynamic_context_count, "prompt.dynamic_context_count"),
    latest_dynamic_sequence: readNullableU64(
      object.latest_dynamic_sequence,
      "prompt.latest_dynamic_sequence",
    ),
  };
}

function decodePromptBlock(value: unknown): TrajectoryPromptBlock {
  const object = readObject(value, "prompt block", TRAJECTORY_PROMPT_BLOCK_FIELDS);
  return {
    id: readString(object.id, "prompt block.id"),
    sequence_order: readNumber(object.sequence_order, "prompt block.sequence_order"),
    content: readString(object.content, "prompt block.content"),
    truncated: readBoolean(object.truncated, "prompt block.truncated"),
  };
}

function decodeToolSpec(value: unknown): ToolSpec {
  const object = readObject(value, "tool spec", TOOL_SPEC_FIELDS);
  return {
    name: readToolName(object.name, "tool spec.name"),
    description: readString(object.description, "tool spec.description"),
    input_schema: readJsonSchema(object.input_schema, "tool spec.input_schema"),
  };
}

function decodePayload(value: unknown): TrajectoryPayload {
  const object = readObject(value, "trajectory payload", TRAJECTORY_PAYLOAD_FIELDS);
  return {
    kind: readEnum(object.kind, TRAJECTORY_PAYLOAD_KIND_VALUES, "payload.kind"),
    content: readString(object.content, "payload.content"),
    truncated: readBoolean(object.truncated, "payload.truncated"),
  };
}

function decodeArtifact(value: unknown): ArtifactReference {
  const object = readObject(value, "artifact reference", ARTIFACT_REF_FIELDS);
  return {
    id: readString(object.id, "artifact.id"),
    kind: readEnum(object.kind, ARTIFACT_KIND_VALUES, "artifact.kind"),
    label: readNullableString(object.label, "artifact.label"),
  };
}

function decodeDiagnostic(value: unknown): Diagnostic {
  const object = readObject(value, "diagnostic", ERROR_INFO_FIELDS);
  return {
    code: readString(object.code, "diagnostic.code"),
    message: readString(object.message, "diagnostic.message"),
  };
}

function readJsonObject(value: unknown, field: string): JsonObject {
  const object = readObject(value, field);
  const result: Record<string, JsonValue> = {};
  for (const [key, child] of Object.entries(object)) {
    result[key] = readJsonValue(child, `${field}.${key}`);
  }
  return result;
}

function readJsonSchema(value: unknown, field: string): JsonSchemaValue {
  if (typeof value === "boolean") {
    return value;
  }
  return readJsonObject(value, field);
}

function readJsonValue(value: unknown, field: string): JsonValue {
  if (value === null || typeof value === "boolean" || typeof value === "string") {
    return value;
  }
  if (typeof value === "number" && Number.isFinite(value)) {
    return value;
  }
  if (Array.isArray(value)) {
    return value.map((child: unknown, index: number) => readJsonValue(child, `${field}[${index}]`));
  }
  return readJsonObject(value, field);
}

function readObject(
  value: unknown,
  field: string,
  allowedKeys?: readonly string[],
): Record<string, unknown> {
  if (!isObject(value)) {
    throw new Error(`${field} must be an object`);
  }
  if (allowedKeys !== undefined) {
    assertKeys(value, allowedKeys, field);
  }
  return value;
}

function assertKeys(value: Record<string, unknown>, allowedKeys: readonly string[], field: string): void {
  const allowed = new Set(allowedKeys);
  for (const key of Object.keys(value)) {
    if (!allowed.has(key)) {
      throw new Error(`${field} contains unsupported field: ${key}`);
    }
  }
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function readArray(value: unknown, field: string): readonly unknown[] {
  if (!Array.isArray(value)) {
    throw new Error(`${field} must be an array`);
  }
  return value;
}

function readString(value: unknown, field: string): string {
  if (typeof value !== "string") {
    throw new Error(`${field} must be a string`);
  }
  return value;
}

function readBoolean(value: unknown, field: string): boolean {
  if (typeof value !== "boolean") {
    throw new Error(`${field} must be a boolean`);
  }
  return value;
}

function readNullableString(value: unknown, field: string): string | null {
  return value === undefined || value === null ? null : readString(value, field);
}

function readToolName(value: unknown, field: string): string {
  const name = readString(value, field);
  if (name.length === 0 || name.length > 64 || !/^[A-Za-z_][A-Za-z0-9_-]*$/.test(name)) {
    throw new Error(`${field} has an invalid tool name`);
  }
  return name;
}

function readNullableToolName(value: unknown, field: string): string | null {
  return value === undefined || value === null ? null : readToolName(value, field);
}

function readNumber(value: unknown, field: string): number {
  if (
    typeof value !== "number"
    || !Number.isSafeInteger(value)
    || value < 0
    || value > 4_294_967_295
  ) {
    throw new Error(`${field} must be a non-negative integer`);
  }
  return value;
}

function readU64(value: unknown, field: string): WireInteger {
  if (typeof value === "string" && /^\d+$/.test(value)) {
    try {
      return BigInt(value);
    } catch {
      throw new Error(`${field} must be a non-negative integer`);
    }
  }
  if (typeof value === "number" && Number.isSafeInteger(value) && value >= 0) {
    return BigInt(value);
  }
  throw new Error(`${field} must be a non-negative integer string`);
}

function readNullableU64(value: unknown, field: string): WireInteger | null {
  return value === undefined || value === null ? null : readU64(value, field);
}

function readEnum<T extends string>(value: unknown, choices: readonly T[], field: string): T {
  if (typeof value !== "string") {
    throw new Error(`${field} has an unsupported value`);
  }
  const match = choices.find((choice: T) => choice === value);
  if (match === undefined) {
    throw new Error(`${field} has an unsupported value`);
  }
  return match;
}
