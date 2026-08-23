import type { TrajectoryRecord, WireInteger } from "./trajectory-contract.js";

/** Records that belong to the visible conversation message list. */
export function isConversationMessage(record: TrajectoryRecord): boolean {
  return record.kind === "user_input"
    || record.kind === "assistant_message"
    || record.kind === "tool_call"
    || record.kind === "tool_result";
}

/** Stable ordering for records that share a journal sequence. */
export function compareRecords(left: TrajectoryRecord, right: TrajectoryRecord): number {
  return compareIntegers(left.start_sequence, right.start_sequence)
    || left.sequence_order - right.sequence_order
    || left.id.localeCompare(right.id);
}

export interface MessageGroup {
  readonly key: string;
  readonly number: number;
  readonly startSequence: WireInteger;
  readonly endSequence: WireInteger;
  readonly records: readonly TrajectoryRecord[];
  readonly messages: readonly TrajectoryRecord[];
  readonly lifecycle: readonly TrajectoryRecord[];
}

interface MutableMessageGroup {
  readonly key: string;
  readonly number: number;
  readonly startSequence: WireInteger;
  readonly records: TrajectoryRecord[];
}

/** Groups all records following a user message into one conversation turn. */
export function buildMessageGroups(records: readonly TrajectoryRecord[]): readonly MessageGroup[] {
  const ordered = [...records].sort(compareRecords);
  const turnStarts = [...new Set(
    ordered
      .filter((record) => record.kind === "user_input")
      .map((record) => record.turn_id ?? record.start_sequence),
  )].sort(compareIntegers);
  const groups = new Map<string, MutableMessageGroup>();
  const prelude = createGroup("prelude", 0, 0n);

  for (const record of ordered) {
    const turnId = record.turn_id;
    const fallbackTurn = turnId === null
      ? latestTurnStart(turnStarts, record.start_sequence)
      : turnId;
    const group = fallbackTurn === null
      ? prelude
      : groups.get(turnKey(fallbackTurn)) ?? createGroup(
        turnKey(fallbackTurn),
        turnStarts.indexOf(fallbackTurn) + 1,
        record.start_sequence,
      );
    groups.set(group.key, group);
    group.records.push(record);
  }

  return [...groups.values(), ...(prelude.records.length > 0 ? [prelude] : [])]
    .sort((left, right) => left.number - right.number || compareIntegers(left.startSequence, right.startSequence))
    .map(finalizeGroup);
}

function createGroup(key: string, number: number, startSequence: WireInteger): MutableMessageGroup {
  return { key, number, startSequence, records: [] };
}

function latestTurnStart(turnStarts: readonly WireInteger[], sequence: WireInteger): WireInteger | null {
  let latest: WireInteger | null = null;
  for (const start of turnStarts) {
    if (start > sequence) {
      break;
    }
    latest = start;
  }
  return latest;
}

function turnKey(sequence: WireInteger): string {
  return `turn-${sequence}`;
}

function finalizeGroup(group: MutableMessageGroup): MessageGroup {
  const records = [...group.records].sort(compareRecords);
  return {
    key: group.key,
    number: group.number,
    startSequence: group.startSequence,
    endSequence: records.reduce(
      (latest, record) => maxInteger(latest, record.end_sequence ?? record.start_sequence),
      group.startSequence,
    ),
    records,
    messages: records.filter(isConversationMessage),
    lifecycle: records.filter((record) => record.kind === "lifecycle"),
  };
}

function compareIntegers(left: WireInteger, right: WireInteger): number {
  return left < right ? -1 : left > right ? 1 : 0;
}

function maxInteger(left: WireInteger, right: WireInteger): WireInteger {
  return left > right ? left : right;
}

/** Returns whether a record's visible or diagnostic evidence matches a query. */
export function matchesRecordQuery(record: TrajectoryRecord, query: string): boolean {
  const normalized = query.trim().toLocaleLowerCase();
  if (normalized.length === 0) {
    return true;
  }
  const details = record.details.type === "message"
    ? record.details.content
    : record.details.type === "tool"
      ? `${record.details.tool.arguments_json} ${record.details.tool.output?.content ?? ""}`
      : "";
  return [record.label, record.summary ?? "", record.kind, details, record.diagnostic?.message ?? ""]
    .join(" ")
    .toLocaleLowerCase()
    .includes(normalized);
}

/** Returns whether a message group contains matching conversation evidence. */
export function matchesMessageGroup(group: MessageGroup, query: string): boolean {
  return group.records.some((record) => matchesRecordQuery(record, query));
}
