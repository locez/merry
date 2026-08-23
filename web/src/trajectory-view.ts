import type {
  ArtifactReference,
  Diagnostic,
  JsonValue,
  ToolSpec,
  TrajectoryLane,
  TrajectoryPromptSnapshot,
  TrajectoryRecord,
  TrajectorySnapshot,
  WireInteger,
} from "./trajectory-contract.js";
import {
  buildMessageGroups,
  compareRecords,
  isConversationMessage,
  matchesMessageGroup,
  matchesRecordQuery,
  type MessageGroup,
} from "./trajectory-message-model.js";
import { buildTimelineScale, type TimelineScale } from "./trajectory-timeline.js";
import { formatJsonContent, renderCodeContent } from "./trajectory-format.js";

export type ConnectionState = "connecting" | "live" | "closed" | "error";
export type ViewMode = "sequence" | "turns" | "calls";
export type InspectorTab = "summary" | "payload" | "result" | "schema" | "timing";

export interface ViewState {
  snapshot: TrajectorySnapshot | null;
  selectedRecordId: string | null;
  connection: ConnectionState;
  mode: ViewMode;
  inspectorTab: InspectorTab;
  query: string;
}

export type ViewAction =
  | { readonly type: "select"; readonly recordId: string; readonly focusLedger: boolean }
  | { readonly type: "clear_selection" }
  | { readonly type: "set_mode"; readonly mode: ViewMode }
  | { readonly type: "set_tab"; readonly tab: InspectorTab }
  | { readonly type: "set_query"; readonly query: string }
  | { readonly type: "refresh" };

const laneOrder: readonly TrajectoryLane[] = ["input", "model", "tools", "system"];
const laneNames: Readonly<Record<TrajectoryLane, string>> = {
  input: "Input",
  model: "Model",
  tools: "Tools",
  system: "System",
};
const laneShortNames: Readonly<Record<TrajectoryLane, string>> = {
  input: "INPUT",
  model: "MODEL",
  tools: "TOOL",
  system: "SYSTEM",
};
const modeNames: Readonly<Record<ViewMode, string>> = {
  sequence: "Sequence",
  turns: "Turns",
  calls: "Calls",
};
const inspectorTabs: readonly InspectorTab[] = [
  "summary",
  "payload",
  "result",
  "schema",
  "timing",
];
const inspectorTabNames: Readonly<Record<InspectorTab, string>> = {
  summary: "Summary",
  payload: "Payload",
  result: "Result",
  schema: "Schema",
  timing: "Timing",
};

function recordBadgeLabel(record: TrajectoryRecord): string {
  return laneShortNames[record.lane];
}

export function renderTrajectory(
  root: HTMLElement,
  sessionId: string,
  state: ViewState,
  dispatch: (action: ViewAction) => void,
): void {
  const snapshot = state.snapshot;
  if (snapshot === null) {
    root.innerHTML = renderLoading(sessionId);
    bindRefresh(root, dispatch);
    return;
  }

  const records = [...snapshot.records].sort(compareRecords);
  const groups = buildMessageGroups(records);
  const selected = records.find((record: TrajectoryRecord) => record.id === state.selectedRecordId);
  const activeRecords = records.filter((record: TrajectoryRecord) => modeIncludes(record, state.mode));
  const matchingRecords = activeRecords.filter((record: TrajectoryRecord) => matchesQuery(record, state.query));
  const matchingGroups = groups.filter((group: MessageGroup) => matchesMessageGroup(group, state.query));

  root.innerHTML = `
    <main class="app-shell">
      ${renderHeader(sessionId, state)}
      ${renderSummary(snapshot, records, groups)}
      ${renderOverview(snapshot, activeRecords, state)}
      <div class="workspace-grid">
        ${renderLedger(activeRecords, matchingRecords, matchingGroups, state, snapshot.tool_specs)}
        <aside class="inspector-panel" aria-label="Record inspector">
          ${selected === undefined ? renderEmptyInspector() : renderInspector(selected, state.inspectorTab, snapshot.tool_specs, sessionId)}
        </aside>
      </div>
    </main>`;

  bindActions(root, dispatch);
}

function renderLoading(sessionId: string): string {
  return `
    <main class="loading-shell">
      <div class="brand-mark">M</div>
      <p class="eyebrow">MERRY / OBSERVABILITY</p>
      <h1>Trajectory</h1>
      <code class="loading-session">${escapeHtml(sessionId)}</code>
      <p class="muted">Connecting to the local session stream...</p>
    </main>`;
}

export function renderError(sessionId: string, message: string): string {
  return `
    <main class="loading-shell">
      <div class="brand-mark brand-mark-error">!</div>
      <p class="eyebrow">MERRY / OBSERVABILITY</p>
      <h1>Trajectory</h1>
      <code class="loading-session">${escapeHtml(sessionId)}</code>
      <p class="error-copy">${escapeHtml(message)}</p>
      <button class="button button-primary" type="button" data-action="refresh">Refresh</button>
    </main>`;
}

function renderHeader(sessionId: string, state: ViewState): string {
  return `
    <header class="app-header">
      <div class="brand-lockup">
        <div class="brand-mark">M</div>
        <div>
          <p class="eyebrow">MERRY / OBSERVABILITY</p>
          <h1>Trajectory</h1>
        </div>
      </div>
      <div class="header-meta">
        <span class="connection connection-${state.connection}">
          <span class="connection-dot"></span>${connectionLabel(state.connection)}
        </span>
        <code class="session-id" title="Session ID">${escapeHtml(sessionId)}</code>
        <button class="icon-button" type="button" data-action="refresh" aria-label="Refresh trajectory" title="Refresh trajectory">&#8635;</button>
      </div>
    </header>`;
}

function renderSummary(
  snapshot: TrajectorySnapshot,
  records: readonly TrajectoryRecord[],
  groups: readonly MessageGroup[],
): string {
  const turns = groups.filter((group) => group.number > 0).length;
  const calls = records.filter((record: TrajectoryRecord) => record.lane === "tools").length;
  return `
    <section class="summary-strip" aria-label="Session summary">
      ${summaryMetric("Revision", snapshot.revision.toString(), "pink")}
      ${summaryMetric("Sequence", snapshot.latest_sequence.toString(), "mint")}
      ${summaryMetric("Turns", turns.toString(), "lavender")}
      ${summaryMetric("Calls", calls.toString(), "orange")}
      <div class="summary-spacer"></div>
      <span class="summary-note">${records.length} records${snapshot.history_truncated_before === null ? "" : ` &middot; history before #${snapshot.history_truncated_before} is bounded`} &middot; journal-backed</span>
    </section>`;
}

function summaryMetric(label: string, value: string, tone: string): string {
  return `
    <div class="summary-metric">
      <span class="metric-mark metric-${tone}"></span>
      <span class="summary-label">${escapeHtml(label)}</span>
      <strong>${escapeHtml(value)}</strong>
    </div>`;
}

function renderOverview(
  snapshot: TrajectorySnapshot,
  records: readonly TrajectoryRecord[],
  state: ViewState,
): string {
  const timeline = buildTimelineScale(records, snapshot.latest_sequence);
  return `
    <section class="overview-panel" aria-label="Trajectory overview">
      <div class="section-heading overview-heading">
        <div>
          <p class="eyebrow">MESSAGE TIMELINE</p>
          <h2>Conversation timeline</h2>
        </div>
        <div class="overview-meta">
          <span class="mode-caption">${modeNames[state.mode].toUpperCase()}</span>
          <span class="timing-caption">Timing not recorded</span>
        </div>
      </div>
      <div class="plot-area">
        <div class="plot-axis-label">VISIBLE JOURNAL EVENTS</div>
        <div class="plot-axis">
          ${timeline.ticks.map((tick) => `<span class="axis-tick" style="left:${tick.position}%">${tick.sequence}</span>`).join("")}
        </div>
        ${laneOrder.map((lane: TrajectoryLane) => renderPlotLane(lane, records, timeline, state)).join("")}
      </div>
      <div class="plot-legend">
        ${laneOrder.map((lane: TrajectoryLane) => `<span><i class="legend-dot dot-${lane}"></i>${laneNames[lane]}</span>`).join("")}
        <span class="legend-spacer"></span>
        <span><i class="legend-outline"></i>selection</span>
      </div>
    </section>`;
}

function renderPlotLane(
  lane: TrajectoryLane,
  records: readonly TrajectoryRecord[],
  timeline: TimelineScale,
  state: ViewState,
): string {
  const laneRecords = records.filter((record: TrajectoryRecord) => record.lane === lane);
  return `
    <div class="plot-row">
      <div class="plot-row-label"><i class="legend-dot dot-${lane}"></i>${laneNames[lane]}</div>
      <div class="plot-track">
        <span class="plot-baseline"></span>
        ${laneRecords.map((record: TrajectoryRecord) => renderPlotRecord(record, timeline, state)).join("")}
      </div>
    </div>`;
}

function renderPlotRecord(
  record: TrajectoryRecord,
  timeline: TimelineScale,
  state: ViewState,
): string {
  const position = timeline.positions.get(record.id);
  if (position === undefined) {
    return "";
  }
  const selected = state.selectedRecordId === record.id;
  const matching = matchesQuery(record, state.query);
  return `
    <button
      class="plot-span plot-${record.lane} ${selected ? "plot-selected" : ""} ${matching ? "" : "plot-dimmed"}"
      type="button"
      style="left:${position.left}%;width:${position.width}%"
      data-plot-record-id="${escapeHtml(record.id)}"
      aria-label="${escapeHtml(recordTitle(record))}, ${escapeHtml(sequenceLabel(record))}"
      title="${escapeHtml(recordTitle(record))} &middot; ${escapeHtml(sequenceLabel(record))}"></button>`;
}

function renderLedger(
  activeRecords: readonly TrajectoryRecord[],
  matchingRecords: readonly TrajectoryRecord[],
  matchingGroups: readonly MessageGroup[],
  state: ViewState,
  toolSpecs: readonly ToolSpec[],
): string {
  const turnNumbers = buildTurnNumbers(activeRecords);
  const matchingIds = new Set(matchingRecords.map((record: TrajectoryRecord) => record.id));
  const rows = state.mode === "turns"
    ? matchingGroups.map((group) => renderMessageGroup(group, state.query, state, toolSpecs)).join("")
    : activeRecords
      .map((record: TrajectoryRecord) => {
        if (!matchingIds.has(record.id)) {
          return "";
        }
        return renderLedgerRecord(record, turnNumbers.get(record.id) ?? 0, state, undefined, toolSpecs);
      })
      .join("");
  const visibleCount = state.mode === "turns"
    ? matchingGroups.reduce((total, group) => total + group.records.filter((record) => matchesRecordQuery(record, state.query)).length, 0)
    : matchingRecords.length;
  const empty = visibleCount === 0
    ? `<div class="ledger-empty"><span class="empty-mark">&#8998;</span><strong>No matching events</strong></div>`
    : "";
  return `
    <section class="ledger-panel" aria-label="Conversation message list">
      <div class="section-heading ledger-heading">
        <div>
          <p class="eyebrow">MESSAGE LIST</p>
          <h2>${state.mode === "turns" ? `${visibleCount} visible events` : `${matchingRecords.length} visible events`}</h2>
        </div>
        <div class="ledger-tools">
          ${renderModeButtons(state.mode)}
          <label class="search-box">
            <span class="search-icon">&#8998;</span>
            <input type="search" data-action="search" value="${escapeHtml(state.query)}" placeholder="Search events" aria-label="Search events" />
          </label>
        </div>
      </div>
      ${state.mode === "turns" ? renderPromptSummary(state.snapshot?.prompt ?? null) : ""}
      <div class="ledger-columns" aria-hidden="true">
        <span>${state.mode === "turns" ? "TURN" : "SEQ"}</span><span>MESSAGE</span><span>EVIDENCE</span><span>STATUS</span>
      </div>
      <div class="ledger-list">
        ${empty}${rows}
      </div>
    </section>`;
}

function renderPromptSummary(prompt: TrajectoryPromptSnapshot | null): string {
  const stableBlocks = prompt?.stable_blocks ?? [];
  const contextCount = prompt?.dynamic_context_count ?? 0n;
  const stableLabel = stableBlocks.length === 0
    ? "No stable prompt snapshot"
    : "Stable instructions";
  const stableDescription = stableBlocks.length === 0
    ? "Provider prompt details are unavailable for this session."
    : `${stableBlocks.length} blocks retained once · ${contextCount} dynamic context messages observed`;
  return `
    <details class="prompt-summary">
      <summary class="prompt-summary-toggle">
        <span class="event-badge badge-system">SYSTEM PROMPT</span>
        <span class="prompt-summary-copy"><strong>${stableLabel}</strong><small>${stableDescription}</small></span>
        <span class="prompt-summary-action">${stableBlocks.length === 0 ? "Unavailable" : "View prompt"}</span>
      </summary>
      ${stableBlocks.length === 0 ? "" : `<div class="prompt-block-list">
        ${stableBlocks.map((block) => `<details class="prompt-block"><summary>Block ${block.sequence_order + 1}</summary><pre>${escapeHtml(block.content)}</pre></details>`).join("")}
      </div>`}
      ${stableBlocks.length === 0 ? "" : `<span class="prompt-context-note">Stable instructions are shown once for the session; dynamic context is represented by the turn records below.</span>`}
    </details>`;
}

function renderMessageGroup(group: MessageGroup, query: string, state: ViewState, toolSpecs: readonly ToolSpec[]): string {
  const records = group.records.filter((record) => matchesRecordQuery(record, query));
  if (records.length === 0) {
    return "";
  }
  const messageCount = records.filter(isConversationMessage).length;
  const toolCount = records.filter((record) => record.lane === "tools").length;
  const lifecycleCount = records.filter((record) => record.kind === "lifecycle").length;
  const details = [
    messageCount === 0 ? "" : `${messageCount} message${messageCount === 1 ? "" : "s"}`,
    toolCount === 0 ? "" : `${toolCount} tool${toolCount === 1 ? "" : "s"}`,
    lifecycleCount === 0 ? "" : `${lifecycleCount} lifecycle event${lifecycleCount === 1 ? "" : "s"}`,
  ].filter((value) => value.length > 0).join(" · ");
  const turnLabel = group.number === 0 ? "HISTORY" : `T${group.number.toString().padStart(2, "0")}`;
  const title = group.number === 0 ? "Session history" : `Turn ${group.number}`;
  const rowCaption = group.number === 0 ? "HISTORY" : `T${group.number.toString().padStart(2, "0")}`;
  return `
    <section class="message-group" aria-label="${escapeHtml(title)}">
      <div class="turn-header">
        <span class="turn-marker">${turnLabel}</span>
        <span class="turn-header-copy"><strong>${escapeHtml(title)}</strong><small>${escapeHtml(details)}</small></span>
        <span class="turn-sequence">${sequenceLabelForRange(group.startSequence, group.endSequence)}</span>
      </div>
      ${records.map((record) => renderLedgerRecord(record, group.number, state, rowCaption, toolSpecs)).join("")}
    </section>`;
}

function renderModeButtons(activeMode: ViewMode): string {
  return `
    <div class="mode-switch" role="tablist" aria-label="Trajectory view mode">
      ${(["sequence", "turns", "calls"] as const)
        .map(
          (mode: ViewMode) => `
            <button class="mode-button ${activeMode === mode ? "mode-active" : ""}" type="button" role="tab" aria-selected="${activeMode === mode}" data-action="mode" data-mode="${mode}">${modeNames[mode]}</button>`,
        )
        .join("")}
    </div>`;
}

function renderLedgerRecord(
  record: TrajectoryRecord,
  turn: number,
  state: ViewState,
  turnCaption?: string,
  toolSpecs: readonly ToolSpec[] = [],
): string {
  const selected = state.selectedRecordId === record.id;
  const evidence = renderEvidence(record);
  return `
    <button class="ledger-row message-row ${selected ? "ledger-row-selected" : ""}" type="button" data-ledger-record-id="${escapeHtml(record.id)}">
      <span class="ledger-sequence">
        <strong>${record.start_sequence.toString().padStart(2, "0")}</strong>
        <small>${escapeHtml(turnCaption ?? (turn === 0 ? "SYS" : `T${turn.toString().padStart(2, "0")}`))}</small>
      </span>
      <span class="ledger-event-label">
          <span class="event-badge badge-${record.lane}">${recordBadgeLabel(record)}</span>
        <span class="ledger-title-block">
          <strong>${escapeHtml(recordTitle(record, toolSpecs))}</strong>
          <span>${escapeHtml(recordSubtitle(record, toolSpecs))}</span>
        </span>
      </span>
      <span class="ledger-evidence">${evidence}</span>
      <span class="ledger-status"><i class="status-dot status-${record.status}"></i>${escapeHtml(statusLabel(record.status))}</span>
    </button>`;
}

function renderEvidence(record: TrajectoryRecord): string {
  if (record.details.type === "tool") {
    const argumentText = textPreview(
      record.details.tool.arguments_json || jsonPretty(record.details.tool.arguments),
      220,
    );
    const output = record.details.tool.output;
    return `
      <span class="evidence-pair"><span class="evidence-label">args</span><code>${escapeHtml(argumentText)}</code></span>
      ${output === null ? "" : `<span class="evidence-arrow">&rarr;</span><span class="evidence-pair"><span class="evidence-label evidence-result">result</span><code>${escapeHtml(textPreview(output.content, 180))}</code></span>`}`;
  }
  if (record.details.type === "message") {
    return `<span class="message-preview">${escapeHtml(textPreview(record.details.content, 420))}</span>`;
  }
  if (record.diagnostic !== null) {
    return `<span class="diagnostic-preview">${escapeHtml(record.diagnostic.message)}</span>`;
  }
  return `<span class="message-preview">${escapeHtml(record.summary ?? "Lifecycle event")}</span>`;
}

function renderInspector(
  record: TrajectoryRecord,
  activeTab: InspectorTab,
  toolSpecs: readonly ToolSpec[],
  sessionId: string,
): string {
  return `
    <div class="inspector-shell">
      <div class="inspector-header">
        <div>
          <p class="eyebrow">${escapeHtml(recordBadgeLabel(record))} &middot; SEQ ${record.start_sequence}</p>
          <h2>${escapeHtml(recordTitle(record, toolSpecs))}</h2>
        </div>
        <button class="icon-button inspector-close" type="button" data-action="clear-selection" aria-label="Close inspector" title="Close inspector">&times;</button>
      </div>
      <div class="inspector-tabs" role="tablist" aria-label="Record details">
        ${inspectorTabs.map((tab: InspectorTab) => `<button class="inspector-tab ${tab === activeTab ? "tab-active" : ""}" type="button" role="tab" aria-selected="${tab === activeTab}" data-action="tab" data-tab="${tab}">${inspectorTabNames[tab]}</button>`).join("")}
      </div>
      <div class="inspector-body">
        ${renderInspectorTab(record, activeTab, toolSpecs, sessionId)}
      </div>
    </div>`;
}

function renderInspectorTab(
  record: TrajectoryRecord,
  tab: InspectorTab,
  toolSpecs: readonly ToolSpec[],
  sessionId: string,
): string {
  if (tab === "summary") {
    return renderSummaryTab(record, toolSpecs, sessionId);
  }
  if (tab === "payload") {
    return renderPayloadTab(record);
  }
  if (tab === "result") {
    return renderResultTab(record, sessionId);
  }
  if (tab === "schema") {
    return renderSchemaTab(record, toolSpecs);
  }
  return renderTimingTab(record);
}

function renderSummaryTab(
  record: TrajectoryRecord,
  toolSpecs: readonly ToolSpec[],
  sessionId: string,
): string {
  return `
    <div class="inspector-section first-section">
      <div class="inspector-status-line"><span class="status-pill status-${record.status}">${escapeHtml(statusLabel(record.status))}</span><span class="kind-label">${escapeHtml(record.kind.replaceAll("_", " "))}</span></div>
      <dl class="detail-list">
        ${detail("Hierarchy", record.parent_id === null ? "Session root" : record.parent_id)}
        ${detail("Lane", laneNames[record.lane])}
        ${detail("Sequence", sequenceLabel(record))}
        ${detail("Record ID", record.id, true)}
        ${record.tool_call_id === null ? "" : detail("Tool call ID", record.tool_call_id, true)}
      </dl>
    </div>
    ${renderSummaryEvidence(record, toolSpecs)}
    ${renderDiagnostic(record.diagnostic)}
    ${renderArtifacts(record.artifacts, sessionId)}`;
}

function renderSummaryEvidence(record: TrajectoryRecord, toolSpecs: readonly ToolSpec[]): string {
  if (record.details.type === "tool") {
    const output = record.details.tool.output;
    return `
      ${codeSection("Payload", formatJsonContent(record.details.tool.arguments_json || jsonPretty(record.details.tool.arguments)), "json")}
      ${output === null ? emptyInspectorSection("No result recorded yet.") : `${codeSection("Result", output.content, output.kind)}${truncationNotice(output.truncated)}`}
      <div class="summary-facts">
        <span><small>Schema</small><strong>${resolveToolSpec(record, toolSpecs) === null ? "Unavailable" : "Available"}</strong></span>
        <span><small>Timing</small><strong>${record.started_at_ms === null ? "Not recorded" : "Recorded"}</strong></span>
      </div>`;
  }
  if (record.details.type === "message") {
    return `${codeSection("Message", record.details.content, "text")}${truncationNotice(record.details.truncated)}`;
  }
  return record.summary === null
    ? ""
    : codeSection("Event", record.summary, "text");
}

function renderPayloadTab(record: TrajectoryRecord): string {
  if (record.details.type === "tool") {
    return codeSection("Arguments", formatJsonContent(record.details.tool.arguments_json || jsonPretty(record.details.tool.arguments)), "json");
  }
  if (record.details.type === "message") {
    return codeSection("Message", record.details.content, "text") + truncationNotice(record.details.truncated);
  }
  return record.summary === null
    ? emptyInspectorSection("No payload recorded for this event.")
    : codeSection("Event", record.summary, "text");
}

function renderResultTab(record: TrajectoryRecord, sessionId: string): string {
  if (record.details.type !== "tool" || record.details.tool.output === null) {
    return `${emptyInspectorSection("No tool result is attached to this event.")}${renderArtifacts(record.artifacts, sessionId)}`;
  }
  const output = record.details.tool.output;
  return `${codeSection("Tool result", output.content, output.kind)}${truncationNotice(output.truncated)}${renderArtifacts(record.artifacts, sessionId)}`;
}

function renderSchemaTab(record: TrajectoryRecord, toolSpecs: readonly ToolSpec[]): string {
  const spec = resolveToolSpec(record, toolSpecs);
  if (spec === null) {
    return emptyInspectorSection("Schema unavailable for this event.");
  }
  return `
    <div class="schema-heading"><span class="schema-name">${escapeHtml(spec.name)}</span><p>${escapeHtml(spec.description)}</p></div>
    ${codeSection("Input schema", jsonPretty(spec.input_schema), "json")}`;
}

function renderTimingTab(record: TrajectoryRecord): string {
  const hasTiming = record.started_at_ms !== null && record.finished_at_ms !== null;
  const duration = hasTiming && record.finished_at_ms !== null && record.started_at_ms !== null
    ? `${maxInteger(record.finished_at_ms - record.started_at_ms, 0n)} ms`
    : "Timing not recorded";
  return `
    <div class="timing-summary ${hasTiming ? "timing-available" : "timing-missing"}">
      <span class="eyebrow">DURATION</span>
      <strong>${escapeHtml(duration)}</strong>
    </div>
    <dl class="detail-list">
      ${detail("Started", record.started_at_ms === null ? "Not recorded" : formatTimestamp(record.started_at_ms))}
      ${detail("Finished", record.finished_at_ms === null ? "Not recorded" : formatTimestamp(record.finished_at_ms))}
      ${detail("Sequence span", sequenceLabel(record))}
    </dl>`;
}

function renderDiagnostic(diagnostic: Diagnostic | null): string {
  return diagnostic === null
    ? ""
    : `<div class="diagnostic-box"><span>${escapeHtml(diagnostic.code)}</span><p>${escapeHtml(diagnostic.message)}</p></div>`;
}

function renderArtifacts(artifacts: readonly ArtifactReference[], sessionId?: string): string {
  if (artifacts.length === 0) {
    return "";
  }
  return `
    <div class="inspector-section">
      <p class="eyebrow">ARTIFACTS</p>
      <ul class="artifact-list">
        ${artifacts.map((artifact: ArtifactReference) => `<li><code>${escapeHtml(artifact.id)}</code><span>${escapeHtml(artifact.label ?? artifact.kind)}</span>${sessionId === undefined ? "" : `<a class="artifact-link" href="${artifactUrl(sessionId, artifact.id)}" target="_blank" rel="noreferrer" aria-label="Open exact artifact" title="Open exact artifact">&#8599;</a>`}</li>`).join("")}
      </ul>
    </div>`;
}

function artifactUrl(sessionId: string, artifactId: string): string {
  return `/api/v1/sessions/${encodeURIComponent(sessionId)}/artifacts/${encodeURIComponent(artifactId)}`;
}

function codeSection(label: string, content: string, kind: string): string {
  const normalizedKind = kind.toLocaleLowerCase() === "json" ? "json" : "text";
  return `
    <div class="inspector-section first-section">
      <div class="code-heading"><p class="eyebrow">${escapeHtml(label)}</p><span>${escapeHtml(kind.toUpperCase())}</span></div>
      <pre class="code-block code-${normalizedKind}"><code>${renderCodeContent(content, normalizedKind)}</code></pre>
    </div>`;
}

function emptyInspectorSection(message: string): string {
  return `<div class="inspector-section first-section inspector-empty-copy"><span class="empty-mark">&#9675;</span><p>${escapeHtml(message)}</p></div>`;
}

function truncationNotice(truncated: boolean): string {
  return truncated ? `<p class="truncation-note">Preview is bounded; the exact artifact remains available.</p>` : "";
}

function detail(label: string, value: string, monospace = false): string {
  return `<div><dt>${escapeHtml(label)}</dt><dd class="${monospace ? "monospace" : ""}">${escapeHtml(value)}</dd></div>`;
}

function renderEmptyInspector(): string {
  return `
    <div class="inspector-empty">
      <span class="empty-mark">+</span>
      <p class="eyebrow">INSPECTOR</p>
      <strong>No event selected</strong>
    </div>`;
}

function buildTurnNumbers(records: readonly TrajectoryRecord[]): ReadonlyMap<string, number> {
  const turns = new Map<string, number>();
  let currentTurn = 0;
  for (const record of records) {
    if (record.kind === "user_input") {
      currentTurn += 1;
    }
    turns.set(record.id, currentTurn);
  }
  return turns;
}

function modeIncludes(record: TrajectoryRecord, mode: ViewMode): boolean {
  return mode !== "calls" || record.lane === "tools";
}

function matchesQuery(record: TrajectoryRecord, query: string): boolean {
  const normalizedQuery = query.trim().toLocaleLowerCase();
  if (normalizedQuery.length === 0) {
    return true;
  }
  return recordSearchText(record).toLocaleLowerCase().includes(normalizedQuery);
}

function recordSearchText(record: TrajectoryRecord): string {
  const details = record.details.type === "tool"
    ? `${record.details.tool.arguments_json} ${record.details.tool.output?.content ?? ""} ${record.details.tool.tool_name ?? ""}`
    : record.details.type === "message" ? record.details.content : "";
  return [record.label, record.summary ?? "", record.kind, record.status, details, record.diagnostic?.message ?? ""].join(" ");
}

function recordTitle(record: TrajectoryRecord, toolSpecs: readonly ToolSpec[] = []): string {
  const spec = resolveToolSpec(record, toolSpecs);
  if (spec !== null) {
    return spec.name;
  }
  if (record.details.type === "tool" && record.details.tool.tool_name !== null) {
    return record.details.tool.tool_name;
  }
  return record.label;
}

function recordSubtitle(record: TrajectoryRecord, toolSpecs: readonly ToolSpec[] = []): string {
  if (record.details.type === "tool") {
    const spec = resolveToolSpec(record, toolSpecs);
    return spec === null ? "tool schema unavailable" : spec.description;
  }
  return record.summary ?? record.kind.replaceAll("_", " ");
}

function resolveToolSpec(record: TrajectoryRecord, toolSpecs: readonly ToolSpec[]): ToolSpec | null {
  if (record.details.type !== "tool" || record.details.tool.tool_name === null) {
    return null;
  }
  const toolName = record.details.tool.tool_name;
  return toolSpecs.find((spec) => spec.name === toolName) ?? null;
}

function sequenceLabel(record: TrajectoryRecord): string {
  return record.end_sequence === null || record.end_sequence === record.start_sequence
    ? `#${record.start_sequence}`
    : `#${record.start_sequence} -> #${record.end_sequence}`;
}

function sequenceLabelForRange(start: WireInteger, end: WireInteger): string {
  return start === end ? `#${start}` : `#${start} -> #${end}`;
}

function statusLabel(status: TrajectoryRecord["status"]): string {
  return status.replaceAll("_", " ");
}

function connectionLabel(connection: ConnectionState): string {
  return connection === "live" ? "LIVE" : connection.toUpperCase();
}

function formatTimestamp(timestamp: WireInteger): string {
  const milliseconds = Number(timestamp);
  if (!Number.isSafeInteger(milliseconds)) {
    return `${timestamp} ms`;
  }
  return new Date(milliseconds).toLocaleString(undefined, {
    dateStyle: "medium",
    timeStyle: "medium",
  });
}

function maxInteger(...values: readonly WireInteger[]): WireInteger {
  return values.reduce((largest, value) => (value > largest ? value : largest), 0n);
}

function jsonPretty(value: JsonValue): string {
  return JSON.stringify(value, null, 2) ?? "null";
}


function textPreview(value: string, limit: number): string {
  const normalized = value.replace(/\s+/g, " ").trim();
  return normalized.length > limit ? `${normalized.slice(0, Math.max(limit - 1, 1))}...` : normalized;
}

function bindActions(root: HTMLElement, dispatch: (action: ViewAction) => void): void {
  bindRefresh(root, dispatch);
  root.querySelectorAll<HTMLButtonElement>("[data-action='mode']").forEach((button) => {
    button.addEventListener("click", () => {
      const mode = button.dataset.mode;
      if (mode === "sequence" || mode === "turns" || mode === "calls") {
        dispatch({ type: "set_mode", mode });
      }
    });
  });
  root.querySelectorAll<HTMLButtonElement>("[data-action='tab']").forEach((button) => {
    button.addEventListener("click", () => {
      const tab = button.dataset.tab;
      if (isInspectorTab(tab)) {
        dispatch({ type: "set_tab", tab });
      }
    });
  });
  root.querySelectorAll<HTMLButtonElement>("[data-ledger-record-id]").forEach((button) => {
    button.addEventListener("click", () => {
      const recordId = button.dataset.ledgerRecordId;
      if (recordId !== undefined) {
        dispatch({ type: "select", recordId, focusLedger: false });
      }
    });
  });
  root.querySelectorAll<HTMLButtonElement>("[data-plot-record-id]").forEach((button) => {
    button.addEventListener("click", () => {
      const recordId = button.dataset.plotRecordId;
      if (recordId !== undefined) {
        dispatch({ type: "select", recordId, focusLedger: true });
      }
    });
  });
  root.querySelector<HTMLButtonElement>("[data-action='clear-selection']")?.addEventListener(
    "click",
    () => dispatch({ type: "clear_selection" }),
  );
  root.querySelector<HTMLInputElement>("[data-action='search']")?.addEventListener(
    "input",
    (event: Event) => dispatch({ type: "set_query", query: (event.target as HTMLInputElement).value }),
  );
}

function bindRefresh(root: HTMLElement, dispatch: (action: ViewAction) => void): void {
  root.querySelectorAll<HTMLButtonElement>("[data-action='refresh']").forEach((button) => {
    button.addEventListener("click", () => dispatch({ type: "refresh" }));
  });
}

function isInspectorTab(value: string | undefined): value is InspectorTab {
  if (value === undefined) {
    return false;
  }
  for (const tab of inspectorTabs) {
    if (tab === value) {
      return true;
    }
  }
  return false;
}

function escapeHtml(value: string): string {
  return value.replace(/[&<>'"]/g, (character: string) => {
    const entities: Readonly<Record<string, string>> = {
      "&": "&amp;",
      "<": "&lt;",
      ">": "&gt;",
      "'": "&#39;",
      '"': "&quot;",
    };
    return entities[character] ?? character;
  });
}
