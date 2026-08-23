import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { decodeEvent, decodeSnapshot } from "../dist/trajectory-contract.js";

const fixture = JSON.parse(
  readFileSync(
    new URL("../../crates/merry-core/tests/fixtures/trajectory-contract.json", import.meta.url),
    "utf8",
  ),
);

test("decodes the canonical Rust trajectory fixture", () => {
  const snapshot = decodeSnapshot(fixture.snapshot);

  assert.equal(snapshot.session_id, "trajectory-contract");
  assert.equal(snapshot.revision, 1n);
  assert.equal(snapshot.latest_sequence, 43n);
  assert.equal(snapshot.closed, false);
  assert.equal(snapshot.prompt.dynamic_context_count, 2n);
  assert.equal(snapshot.prompt.latest_dynamic_sequence, 41n);
  assert.deepEqual(snapshot.tool_specs[0], {
    name: "read_file",
    description: "Read a UTF-8 file.",
    input_schema: {
      type: "object",
      properties: { path: { type: "string" } },
      required: ["path"],
    },
  });

  const [input, tool] = snapshot.records;
  assert.equal(input.start_sequence, 42n);
  assert.equal(input.turn_id, 7n);
  assert.equal(input.details.type, "message");
  assert.equal(tool.start_sequence, 43n);
  assert.equal(tool.end_sequence, 43n);
  assert.equal(tool.parent_id, "input-1");
  assert.equal(tool.tool_call_id, "call-1");
  assert.equal(tool.details.type, "tool");
  assert.equal(tool.details.tool.tool_name, "read_file");
  assert.equal(tool.details.tool.arguments_json, '{"mode":"text","path":"README.md"}');
  assert.deepEqual(tool.details.tool.output, {
    kind: "text",
    content: "file contents",
    truncated: true,
  });

  const events = fixture.events.map((value) => decodeEvent(value));
  assert.deepEqual(
    events.map((event) => event.type),
    ["snapshot", "record_upsert", "prompt_updated", "session_closed"],
  );
  assert.equal(events[1].revision, 2n);
  assert.equal(events[2].prompt.latest_dynamic_sequence, 41n);
  assert.equal(events[3].latest_sequence, 43n);
});

test("accepts legacy safe numeric counters and normalizes them to bigint", () => {
  const snapshot = decodeSnapshot({
    ...fixture.snapshot,
    revision: 1,
    latest_sequence: 43,
  });

  assert.equal(snapshot.revision, 1n);
  assert.equal(snapshot.latest_sequence, 43n);
});

test("defaults serde-compatible optional fields and accepts boolean tool schemas", () => {
  const record = { ...fixture.snapshot.records[0] };
  for (const field of [
    "summary",
    "turn_id",
    "end_sequence",
    "parent_id",
    "tool_call_id",
    "diagnostic",
    "started_at_ms",
    "finished_at_ms",
  ]) {
    delete record[field];
  }
  const snapshot = decodeSnapshot({
    ...fixture.snapshot,
    tool_specs: [{ ...fixture.snapshot.tool_specs[0], input_schema: false }],
    records: [record],
  });

  assert.equal(snapshot.records[0].summary, null);
  assert.equal(snapshot.records[0].turn_id, null);
  assert.equal(snapshot.records[0].diagnostic, null);
  assert.equal(snapshot.tool_specs[0].input_schema, false);
});

test("rejects fields outside the Rust deny_unknown_fields contract", () => {
  assert.throws(
    () => decodeSnapshot({ ...fixture.snapshot, unexpected: true }),
    /contains unsupported field: unexpected/,
  );
  assert.throws(
    () => decodeSnapshot({
      ...fixture.snapshot,
      records: [{ ...fixture.snapshot.records[0], sequence_order: 4_294_967_296 }],
    }),
    /record\.sequence_order must be a non-negative integer/,
  );
});
