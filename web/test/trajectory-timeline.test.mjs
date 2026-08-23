import assert from "node:assert/strict";
import test from "node:test";
import { buildTimelineScale } from "../dist/trajectory-timeline.js";

function record(id, sequence, order = 0) {
  return { id, start_sequence: BigInt(sequence), sequence_order: order };
}

test("timeline positions visible sequence groups without reserving hidden gaps", () => {
  const scale = buildTimelineScale([
    record("first", 1),
    record("batch-a", 8, 0),
    record("batch-b", 8, 1),
    record("last", 22),
  ], 22n);

  const first = scale.positions.get("first");
  const batchA = scale.positions.get("batch-a");
  const batchB = scale.positions.get("batch-b");
  const last = scale.positions.get("last");
  assert.ok(first !== undefined);
  assert.ok(batchA !== undefined);
  assert.ok(batchB !== undefined);
  assert.ok(last !== undefined);
  assert.ok(first.left < batchA.left);
  assert.ok(batchA.left < batchB.left);
  assert.ok(batchB.left < last.left);
  assert.ok(last.left + last.width <= 100);
  assert.deepEqual(scale.ticks.map((tick) => tick.sequence), [0n, 1n, 8n, 22n]);
});

test("empty timelines still expose the journal endpoint", () => {
  const scale = buildTimelineScale([], 22n);

  assert.deepEqual(scale.ticks, [
    { sequence: 0n, position: 0 },
    { sequence: 22n, position: 100 },
  ]);
  assert.equal(scale.positions.size, 0);
});
