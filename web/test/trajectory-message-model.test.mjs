import assert from "node:assert/strict";
import test from "node:test";
import { decodeEvent } from "../dist/trajectory-contract.js";
import { formatJsonContent } from "../dist/trajectory-format.js";
import {
  buildMessageGroups,
  compareRecords,
  matchesMessageGroup,
} from "../dist/trajectory-message-model.js";

function message(id, kind, sequence, turnId, text) {
  return {
    id,
    lane: kind === "user_input" ? "input" : "model",
    kind,
    label: kind === "user_input" ? "User input" : "Assistant message",
    summary: text,
    status: "succeeded",
    start_sequence: String(sequence),
    sequence_order: 0,
    turn_id: turnId === null ? null : String(turnId),
    end_sequence: null,
    parent_id: null,
    tool_call_id: null,
    artifacts: [],
    diagnostic: null,
    started_at_ms: null,
    finished_at_ms: null,
    details: { type: "message", content: text, truncated: false },
  };
}

test("message groups use runtime turn ids instead of event kinds", () => {
  const records = [
    message("assistant-1", "assistant_message", 3, 1, "answer"),
    message("user-2", "user_input", 4, 2, "follow-up"),
    message("user-1", "user_input", 1, 1, "question"),
  ];
  const groups = buildMessageGroups(records);

  assert.deepEqual(groups.map((group) => group.number), [1, 2]);
  assert.deepEqual(groups[0].messages.map((record) => record.id), ["user-1", "assistant-1"]);
  assert.equal(groups[1].messages[0].id, "user-2");
});

test("record ordering uses explicit same-sequence order", () => {
  const first = message("first", "assistant_message", 8, 1, "first");
  const second = { ...message("second", "assistant_message", 8, 1, "second"), sequence_order: 1 };
  assert.ok(compareRecords(first, second) < 0);
});

test("lifecycle records stay visible inside their turn group", () => {
  const lifecycle = {
    ...message("lifecycle-1", "lifecycle", 2, 1, "Plan execution started"),
    lane: "system",
    label: "Plan execution",
    details: { type: "none" },
  };
  const groups = buildMessageGroups([
    message("user-1", "user_input", 1, 1, "inspect"),
    lifecycle,
  ]);

  assert.equal(groups[0].records.length, 2);
  assert.equal(groups[0].lifecycle[0].id, "lifecycle-1");
  assert.equal(matchesMessageGroup(groups[0], "plan execution"), true);
});

test("turn groups render by logical turn number when records arrive out of order", () => {
  const groups = buildMessageGroups([
    message("user-1", "user_input", 1, 1, "turn one"),
    message("user-2", "user_input", 39, 2, "turn two"),
    message("user-3", "user_input", 102, 3, "turn three"),
    message("user-4", "user_input", 380, 4, "turn four"),
    message("user-6", "user_input", 130, 6, "later input"),
    message("user-5", "user_input", 395, 5, "earlier logical turn"),
  ]);

  assert.deepEqual(groups.map((group) => group.number), [1, 2, 3, 4, 5, 6]);
  assert.deepEqual(groups.slice(-2).map((group) => group.messages[0].id), ["user-5", "user-6"]);
});

test("prompt updates decode as a session-level event", () => {
  const event = decodeEvent({
    type: "prompt_updated",
    revision: 4,
    latest_sequence: 9,
    prompt: {
      stable_blocks: [{ id: "prompt-1", sequence_order: 0, content: "stable", truncated: false }],
      dynamic_context_count: 2,
      latest_dynamic_sequence: 9,
    },
  });

  assert.equal(event.type, "prompt_updated");
  assert.equal(event.prompt.dynamic_context_count, 2n);
});

test("trajectory counters decode losslessly from decimal strings", async () => {
  const { decodeSnapshot } = await import("../dist/trajectory-contract.js");
  const snapshot = decodeSnapshot({
    session_id: "session-1",
    revision: "9007199254740993",
    latest_sequence: "18446744073709551615",
    closed: true,
    history_truncated_before: null,
    prompt: {
      stable_blocks: [],
      dynamic_context_count: "2",
      latest_dynamic_sequence: "18446744073709551615",
    },
    tool_specs: [],
    records: [],
  });

  assert.equal(snapshot.revision, 9007199254740993n);
  assert.equal(snapshot.latest_sequence, 18446744073709551615n);
  assert.equal(snapshot.closed, true);
});

test("JSON formatting preserves large numeric lexemes", () => {
  const formatted = formatJsonContent('{"value":9007199254740993,"items":[true,null]}');
  assert.match(formatted, /9007199254740993/);
  assert.match(formatted, /\n  "items": \[/);
});

test("JSON formatting expands arrays of strings without losing source text", () => {
  assert.equal(
    formatJsonContent('{"items":["a","b"]}'),
    '{\n  "items": [\n    "a",\n    "b"\n  ]\n}',
  );
});
