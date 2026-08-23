import { decodeEvent, decodeSnapshot } from "./trajectory-contract.js";
import type { TrajectoryEvent, TrajectoryRecord } from "./trajectory-contract.js";
import { compareRecords, isConversationMessage } from "./trajectory-message-model.js";
import {
  renderError,
  renderTrajectory,
  type ViewAction,
  type ViewState,
} from "./trajectory-view.js";

const appRootElement = document.querySelector<HTMLElement>("#app");
if (appRootElement === null) {
  throw new Error("Merry Web root is missing");
}
const appRoot: HTMLElement = appRootElement;

const sessionId = sessionIdFromPath(window.location.pathname);
const state: ViewState = {
  snapshot: null,
  selectedRecordId: null,
  connection: "connecting",
  mode: "turns",
  inspectorTab: "summary",
  query: "",
};
let eventSource: EventSource | null = null;

void loadTrajectory();

async function loadTrajectory(): Promise<void> {
  eventSource?.close();
  eventSource = null;
  state.connection = "connecting";
  state.snapshot = null;
  state.selectedRecordId = null;
    render();
  try {
    const response = await fetch(
      `/api/v1/sessions/${encodeURIComponent(sessionId)}/trajectory`,
      { headers: { Accept: "application/json" } },
    );
    if (!response.ok) {
      throw new Error(`trajectory request failed with HTTP ${response.status}`);
    }
    const payload: unknown = await response.json();
    state.snapshot = decodeSnapshot(payload);
    state.selectedRecordId = firstRecordId(state.snapshot.records);
    state.connection = state.snapshot.closed ? "closed" : "live";
    render();
    if (!state.snapshot.closed) {
      subscribe();
    }
  } catch (error: unknown) {
    state.connection = "error";
    appRoot.innerHTML = renderError(sessionId, messageFor(error));
    bindErrorActions();
  }
}

function subscribe(): void {
  eventSource = new EventSource(
    `/api/v1/sessions/${encodeURIComponent(sessionId)}/events`,
  );
  eventSource.addEventListener("trajectory", (event: Event) => {
    if (!(event instanceof MessageEvent)) {
      return;
    }
    try {
      const payload: unknown = JSON.parse(event.data);
      applyEvent(decodeEvent(payload));
      render();
    } catch (error: unknown) {
      eventSource?.close();
      state.connection = "error";
      appRoot.innerHTML = renderError(sessionId, messageFor(error));
      bindErrorActions();
    }
  });
  eventSource.addEventListener("error", (event: Event) => {
    if (event instanceof MessageEvent) {
      try {
        const payload: unknown = JSON.parse(event.data);
        if (isResyncError(payload)) {
          eventSource?.close();
          void loadTrajectory();
          return;
        }
      } catch {
        // The browser will report malformed or transport-level SSE errors below.
      }
    }
    if (state.connection !== "closed") {
      state.connection = "error";
      render();
    }
  });
  eventSource.addEventListener("open", () => {
    state.connection = "live";
    render();
  });
}

function applyEvent(event: TrajectoryEvent): void {
  if (event.type === "snapshot") {
    state.snapshot = event.snapshot;
    state.selectedRecordId = state.selectedRecordId ?? firstRecordId(event.snapshot.records);
    state.connection = event.snapshot.closed ? "closed" : "live";
    if (event.snapshot.closed) {
      eventSource?.close();
    }
    return;
  }
  if (state.snapshot === null) {
    return;
  }
  if (event.type !== "session_closed" && event.revision <= state.snapshot.revision) {
    return;
  }
  if (event.type === "session_closed") {
    state.connection = "closed";
    state.snapshot = {
      ...state.snapshot,
      revision: event.revision,
      latest_sequence: event.latest_sequence,
      closed: true,
    };
    return;
  }
  if (event.type === "prompt_updated") {
    state.snapshot = {
      ...state.snapshot,
      revision: event.revision,
      latest_sequence: event.latest_sequence,
      prompt: event.prompt,
    };
    return;
  }
  const records = state.snapshot.records.filter(
    (record: TrajectoryRecord) => record.id !== event.record.id,
  );
  state.snapshot = {
    ...state.snapshot,
    revision: event.revision,
    latest_sequence: event.latest_sequence,
    records: [...records, event.record].sort(compareRecords),
  };
  if (state.selectedRecordId === null) {
    state.selectedRecordId = event.record.id;
  }
}

function dispatch(action: ViewAction): void {
  if (action.type === "refresh") {
    void loadTrajectory();
    return;
  }
  if (action.type === "select") {
    state.selectedRecordId = action.recordId;
    render();
    if (action.focusLedger) {
      window.requestAnimationFrame(() => {
        const row = Array.from(appRoot.querySelectorAll<HTMLElement>("[data-ledger-record-id]")).find(
          (element: HTMLElement) => element.dataset.ledgerRecordId === action.recordId,
        );
        scrollLedgerRowVertically(row);
      });
    }
    return;
  }
  if (action.type === "clear_selection") {
    state.selectedRecordId = null;
    render();
    return;
  }
  if (action.type === "set_mode") {
    state.mode = action.mode;
    render();
    return;
  }
  if (action.type === "set_tab") {
    state.inspectorTab = action.tab;
    render();
    return;
  }
  state.query = action.query;
  render();
  window.requestAnimationFrame(() => {
    const input = appRoot.querySelector<HTMLInputElement>("[data-action='search']");
    input?.focus();
    input?.setSelectionRange(input.value.length, input.value.length);
  });
}

function scrollLedgerRowVertically(row: HTMLElement | undefined): void {
  if (row === undefined) {
    return;
  }
  const bounds = row.getBoundingClientRect();
  const targetTop = window.scrollY + bounds.top - (window.innerHeight - bounds.height) / 2;
  window.scrollTo({
    left: window.scrollX,
    top: Math.max(targetTop, 0),
    behavior: "smooth",
  });
}

function render(): void {
  renderTrajectory(appRoot, sessionId, state, dispatch);
}

function bindErrorActions(): void {
  appRoot.querySelector<HTMLButtonElement>("[data-action='refresh']")?.addEventListener(
    "click",
    () => void loadTrajectory(),
  );
}

function firstRecordId(records: readonly TrajectoryRecord[]): string | null {
  const ordered = [...records].sort(compareRecords);
  const first = ordered.find(isConversationMessage) ?? ordered[0];
  return first?.id ?? null;
}

function sessionIdFromPath(path: string): string {
  const match = /^\/app\/sessions\/([^/]+)\/trajectory\/?$/.exec(path);
  return match?.[1] === undefined ? "unknown-session" : decodeURIComponent(match[1]);
}

function messageFor(error: unknown): string {
  return error instanceof Error ? error.message : "the Web service returned an unknown error";
}

function isResyncError(value: unknown): boolean {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return false;
  }
  const code = (value as { readonly code?: unknown }).code;
  return code === "resync_required";
}
