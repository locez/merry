import type { TrajectoryRecord, WireInteger } from "./trajectory-contract.js";
import { compareRecords } from "./trajectory-message-model.js";

export interface TimelineTick {
  readonly sequence: WireInteger;
  readonly position: number;
}

export interface TimelineRecordPosition {
  readonly left: number;
  readonly width: number;
}

export interface TimelineScale {
  readonly ticks: readonly TimelineTick[];
  readonly positions: ReadonlyMap<string, TimelineRecordPosition>;
}

interface TimelineSequenceGroup {
  readonly sequence: WireInteger;
  readonly records: TrajectoryRecord[];
}

/** Builds a dense visual scale while retaining each record's journal sequence. */
export function buildTimelineScale(
  records: readonly TrajectoryRecord[],
  fallbackLatestSequence: WireInteger,
): TimelineScale {
  const ordered = [...records].sort(compareRecords);
  const groups = groupBySequence(ordered);
  if (groups.length === 0) {
    return {
      ticks: fallbackLatestSequence === 0n
        ? [{ sequence: 0n, position: 0 }]
        : [{ sequence: 0n, position: 0 }, { sequence: fallbackLatestSequence, position: 100 }],
      positions: new Map(),
    };
  }

  const positions = new Map<string, TimelineRecordPosition>();
  const groupWidth = 100 / groups.length;
  groups.forEach((group, groupIndex) => {
    const itemWidth = groupWidth / group.records.length;
    group.records.forEach((record, recordIndex) => {
      const left = groupIndex * groupWidth + itemWidth * recordIndex;
      positions.set(record.id, {
        left,
        width: itemWidth,
      });
    });
  });

  return { ticks: buildTicks(groups), positions };
}

function groupBySequence(records: readonly TrajectoryRecord[]): readonly TimelineSequenceGroup[] {
  const groups: TimelineSequenceGroup[] = [];
  for (const record of records) {
    const current = groups.at(-1);
    if (current === undefined || current.sequence !== record.start_sequence) {
      groups.push({ sequence: record.start_sequence, records: [record] });
      continue;
    }
    current.records.push(record);
  }
  return groups;
}

function buildTicks(groups: readonly TimelineSequenceGroup[]): readonly TimelineTick[] {
  const tickCount = Math.min(groups.length, 5);
  const ticks: TimelineTick[] = [];
  for (let index = 0; index < tickCount; index += 1) {
    const groupIndex = tickCount === 1
      ? 0
      : Math.round((index * (groups.length - 1)) / (tickCount - 1));
    const group = groups[groupIndex];
    if (group === undefined) {
      continue;
    }
    const position = groupIndex === groups.length - 1
      ? 100
      : ((groupIndex + 0.5) / groups.length) * 100;
    ticks.push({ sequence: group.sequence, position });
  }

  const firstGroup = groups[0];
  if (firstGroup !== undefined && firstGroup.sequence > 0n) {
    ticks.unshift({ sequence: 0n, position: 0 });
  }
  return ticks;
}
