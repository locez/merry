use super::{
    keymap::KeyAction,
    plan::{PlanCounts, PlanTreeRow},
    state::TuiState,
    theme::SemanticColor,
};
use merry_core::{
    CoordinatorDirectiveSnapshot, PlanApprovalRequirementKind, PlanAttemptProgressSnapshot,
    PlanAttemptSnapshot, PlanExecutorPolicy, PlanLeaseSnapshot, PlanLinkSnapshot, PlanLinkStatus,
    PlanNodeSnapshot, PlanNodeStatus, PlanPhase, PlanSnapshot,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};

pub(crate) fn render_plan(frame: &mut Frame<'_>, state: &TuiState, region: Rect) {
    if state.plan().is_inspector_open() {
        render_inspector(frame, state, region);
    } else {
        render_tree(frame, state, region);
    }
}

fn render_tree(frame: &mut Frame<'_>, state: &TuiState, region: Rect) {
    let Some(snapshot) = state.plan().snapshot() else {
        return;
    };
    let inner_height = usize::from(region.height.saturating_sub(2));
    let summary_height = usize::from(inner_height > 0);
    let row_capacity = inner_height.saturating_sub(summary_height);
    let rows = state.plan().visible_rows();
    let start = visible_start(
        &rows,
        state.plan().selected_node_id(),
        state.plan().scroll_offset(),
        row_capacity,
    );
    let mut lines = Vec::with_capacity(inner_height);
    if inner_height > 0 {
        lines.push(summary_line(state, state.plan().counts()));
    }
    for row in rows.iter().skip(start) {
        if lines.len() >= inner_height {
            break;
        }
        lines.push(tree_line(state, row));
        if let Some(activity) = row.activity.as_ref()
            && lines.len() < inner_height
        {
            lines.push(activity_line(state, row, activity));
        }
    }

    frame.render_widget(
        Paragraph::new(lines).block(plan_block(state, snapshot, "Plan")),
        region,
    );
}

fn render_inspector(frame: &mut Frame<'_>, state: &TuiState, region: Rect) {
    let Some(snapshot) = state.plan().snapshot() else {
        return;
    };
    let Some(node) = state.plan().selected_node() else {
        return;
    };
    let title = format!("Node {}", compact_id(node.id.as_str(), region.width));
    let lines = inspector_lines(state, snapshot, node);
    let offset = state
        .plan()
        .inspector_scroll_offset()
        .min(usize::from(u16::MAX)) as u16;
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((offset, 0))
            .block(plan_block(state, snapshot, &title)),
        region,
    );
}

fn plan_block<'a>(state: &TuiState, snapshot: &PlanSnapshot, title: &'a str) -> Block<'a> {
    let border_color = if state.plan().is_focused() {
        SemanticColor::Focus
    } else {
        SemanticColor::Muted
    };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .title(Line::from(vec![
            Span::styled(format!(" {title} "), style(state, SemanticColor::Assistant)),
            Span::styled(
                format!("{} r{} ", phase_label(snapshot.phase), snapshot.revision),
                phase_style(state, snapshot.phase).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "[{} toggle] ",
                    state
                        .keymap()
                        .binding_label_for(KeyAction::TogglePlan)
                        .unwrap_or_else(|| "Ctrl+O".to_owned())
                ),
                style(state, SemanticColor::Muted),
            ),
        ]))
        .border_style(style(state, border_color))
}

fn summary_line(state: &TuiState, counts: PlanCounts) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("live {}", counts.live),
            style(state, SemanticColor::Focus),
        ),
        Span::raw("  "),
        Span::styled(
            format!("ready {}", counts.ready),
            style(state, SemanticColor::ToolKeyword),
        ),
        Span::raw("  "),
        Span::styled(
            format!("blocked {}", counts.blocked),
            if counts.blocked > 0 {
                style(state, SemanticColor::Warning)
            } else {
                style(state, SemanticColor::Muted)
            },
        ),
    ])
}

fn tree_line(state: &TuiState, row: &PlanTreeRow) -> Line<'static> {
    let selected = state.plan().selected_node_id() == Some(&row.node_id);
    let selection = selected.then(|| selection_style(state));
    let indent = "  ".repeat(row.depth);
    let fold = match (row.has_children, row.collapsed) {
        (true, true) => "▸",
        (true, false) => "▾",
        (false, _) => " ",
    };
    let status = status_symbol(row.status, row.ready);
    let status_color = status_color(row.status, row.ready);
    let executor = match row.executor_policy {
        PlanExecutorPolicy::Local => "local",
        PlanExecutorPolicy::Delegate => "subagent",
        PlanExecutorPolicy::Auto => "auto",
    };
    let mut spans = vec![
        Span::styled(indent, selection.unwrap_or_default()),
        Span::styled(format!("{fold} "), selection.unwrap_or_default()),
        Span::styled(
            format!("{status} "),
            selection.unwrap_or_else(|| style(state, status_color)),
        ),
        Span::styled(
            row.objective.clone(),
            selection.unwrap_or_else(|| style(state, SemanticColor::Assistant)),
        ),
    ];
    if selected {
        spans.push(Span::styled(
            format!("  {executor}"),
            selection.unwrap_or_default().add_modifier(Modifier::DIM),
        ));
    }
    Line::from(spans)
}

fn activity_line(
    state: &TuiState,
    row: &PlanTreeRow,
    activity: &merry_core::SubagentActivitySnapshot,
) -> Line<'static> {
    let indent = "  ".repeat(row.depth + 1);
    Line::from(vec![
        Span::styled(format!("{indent}↳ "), style(state, SemanticColor::Muted)),
        Span::styled(
            format!("{}  ", activity_phase_label(activity.phase)),
            activity_phase_style(state, activity.phase),
        ),
        Span::styled(
            bounded_activity_summary(&activity.summary),
            style(state, SemanticColor::Muted),
        ),
    ])
}

fn inspector_lines(
    state: &TuiState,
    snapshot: &PlanSnapshot,
    node: &PlanNodeSnapshot,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    section(&mut lines, state, "OBJECTIVE");
    text(&mut lines, state, &node.objective);
    key_value(
        &mut lines,
        state,
        "state",
        &format!(
            "{}  executor {}  node r{}",
            node_status_label(node.status),
            executor_label(node.executor_policy),
            node.updated_revision
        ),
    );

    section(&mut lines, state, "ACCEPTANCE");
    list(
        &mut lines,
        state,
        &node.acceptance,
        "No acceptance checks recorded",
    );

    section(&mut lines, state, "DEPENDENCIES");
    if node.depends_on.is_empty() {
        muted(&mut lines, state, "None");
    } else {
        for dependency in &node.depends_on {
            let status = snapshot
                .nodes
                .iter()
                .find(|candidate| candidate.id == *dependency)
                .map(|candidate| node_status_label(candidate.status))
                .unwrap_or("missing");
            text(
                &mut lines,
                state,
                &format!("- {}  {status}", dependency.as_str()),
            );
        }
    }

    section(&mut lines, state, "HARNESS");
    key_value(
        &mut lines,
        state,
        "model",
        node.harness.model_role.as_deref().unwrap_or("inherited"),
    );
    key_value(
        &mut lines,
        state,
        "reasoning",
        node.harness
            .reasoning_effort
            .as_deref()
            .unwrap_or("inherited"),
    );
    optional_number(
        &mut lines,
        state,
        "checkpoint turns",
        node.harness.checkpoint_turn_interval,
    );
    named_values(
        &mut lines,
        state,
        "tools",
        node.harness.allowed_tools.iter().map(|v| v.as_str()),
    );
    named_values(
        &mut lines,
        state,
        "read",
        node.harness.read_scope.iter().map(String::as_str),
    );
    named_values(
        &mut lines,
        state,
        "write",
        node.harness.write_scope.iter().map(String::as_str),
    );
    named_values(
        &mut lines,
        state,
        "forbidden",
        node.harness.forbidden_paths.iter().map(String::as_str),
    );

    section(&mut lines, state, "LINKED SUBAGENTS");
    if node.links.is_empty() {
        muted(&mut lines, state, "No subagent is linked to this Plan task");
    } else {
        for link in &node.links {
            linked_subagent_lines(&mut lines, state, link);
        }
        if let Some(activity) = state.plan().activity_for_node(node) {
            linked_activity_line(&mut lines, state, activity);
        }
        key_value(
            &mut lines,
            state,
            "runtime summary",
            &format_execution_summary(&node.execution_summary),
        );
    }

    let leases = snapshot
        .leases
        .iter()
        .filter(|lease| lease.node_id == node.id)
        .collect::<Vec<_>>();
    section(&mut lines, state, "LEGACY LEASE EVIDENCE");
    if leases.is_empty() {
        muted(&mut lines, state, "No leases");
    } else {
        for lease in leases {
            lease_lines(&mut lines, state, lease);
        }
    }

    let attempts = snapshot
        .attempts
        .iter()
        .filter(|attempt| attempt.node_id == node.id)
        .collect::<Vec<_>>();
    section(&mut lines, state, "LEGACY ATTEMPTS");
    if attempts.is_empty() {
        muted(&mut lines, state, "No attempts");
    } else {
        for attempt in attempts {
            attempt_lines(&mut lines, state, attempt);
        }
    }

    let progress = snapshot
        .attempt_progress
        .iter()
        .filter(|progress| progress.node_id == node.id)
        .collect::<Vec<_>>();
    section(&mut lines, state, "LEGACY PROGRESS");
    if progress.is_empty() {
        muted(&mut lines, state, "No progress report");
    } else {
        for item in progress {
            progress_lines(&mut lines, state, item);
        }
    }

    let directives = snapshot
        .directives
        .iter()
        .filter(|directive| directive.node_id == node.id)
        .collect::<Vec<_>>();
    section(&mut lines, state, "LEGACY DIRECTIVES");
    if directives.is_empty() {
        muted(&mut lines, state, "None");
    } else {
        for directive in directives {
            directive_lines(&mut lines, state, directive);
        }
    }

    section(&mut lines, state, "APPROVALS");
    if snapshot.approval_requirements.is_empty() {
        muted(&mut lines, state, "No approval requirements");
    } else {
        for approval in &snapshot.approval_requirements {
            text(
                &mut lines,
                state,
                &format!("- {}  {:?}", approval_kind(&approval.kind), approval.status)
                    .to_ascii_lowercase(),
            );
        }
    }

    section(&mut lines, state, "RESULT");
    if let Some(result) = node.result.as_ref() {
        text(&mut lines, state, &result.conclusion);
        list(
            &mut lines,
            state,
            &result.verification,
            "No verification recorded",
        );
        named_values(
            &mut lines,
            state,
            "changed",
            result.changed_paths.iter().map(String::as_str),
        );
        list(
            &mut lines,
            state,
            &result.open_questions,
            "No open questions",
        );
    } else {
        muted(&mut lines, state, "No terminal result");
    }

    section(&mut lines, state, "REVISIONS");
    key_value(
        &mut lines,
        state,
        "node",
        &format!(
            "created r{}  updated r{}",
            node.created_revision, node.updated_revision
        ),
    );
    for revision in snapshot.revision_summaries.iter().rev().take(8) {
        text(
            &mut lines,
            state,
            &format!("- r{} {}", revision.revision(), revision.summary()),
        );
    }
    lines
}

fn linked_subagent_lines(
    lines: &mut Vec<Line<'static>>,
    state: &TuiState,
    link: &PlanLinkSnapshot,
) {
    let status = match link.status {
        PlanLinkStatus::Active => "active",
        PlanLinkStatus::Completed => "completed",
        PlanLinkStatus::Failed => "failed",
        PlanLinkStatus::Cancelled => "cancelled",
        PlanLinkStatus::Superseded => "superseded",
    };
    text(
        lines,
        state,
        &format!(
            "- {status}  agent {}  task {}",
            link.subagent_id.as_str(),
            link.task_id.as_str()
        ),
    );
    muted(
        lines,
        state,
        &format!(
            "  binding {}  linked @ {} ms{}",
            link.binding_id.as_str(),
            link.linked_at_ms,
            link.terminal_at_ms
                .map_or_else(String::new, |at| format!("  terminal @ {at} ms"))
        ),
    );
}

fn linked_activity_line(
    lines: &mut Vec<Line<'static>>,
    state: &TuiState,
    activity: &merry_core::SubagentActivitySnapshot,
) {
    lines.push(Line::from(vec![
        Span::styled("  latest: ".to_owned(), style(state, SemanticColor::Muted)),
        Span::styled(
            format!("{}  ", activity_phase_label(activity.phase)),
            activity_phase_style(state, activity.phase),
        ),
        Span::styled(
            bounded_activity_summary(&activity.summary),
            style(state, SemanticColor::Assistant),
        ),
    ]));
}

fn format_execution_summary(summary: &merry_core::PlanExecutionSummary) -> String {
    format!(
        "active {}  completed {}  failed {}  cancelled {}",
        summary.active, summary.completed, summary.failed, summary.cancelled
    )
}

fn lease_lines(lines: &mut Vec<Line<'static>>, state: &TuiState, lease: &PlanLeaseSnapshot) {
    text(
        lines,
        state,
        &format!(
            "- {:?}  {}  subagent {}",
            lease.status,
            lease.lease_id.as_str(),
            lease.executor_session_id.as_str()
        )
        .to_ascii_lowercase(),
    );
    muted(
        lines,
        state,
        &format!("  heartbeat @ {} ms", lease.last_heartbeat_at_ms),
    );
}

fn attempt_lines(lines: &mut Vec<Line<'static>>, state: &TuiState, attempt: &PlanAttemptSnapshot) {
    let outcome = attempt
        .outcome
        .map(|outcome| format!("{outcome:?}").to_ascii_lowercase())
        .unwrap_or_else(|| "running".to_owned());
    text(
        lines,
        state,
        &format!("- {}  {outcome}", attempt.attempt_id.as_str()),
    );
    if let Some(checkpoint) = attempt.latest_checkpoint_ref.as_deref() {
        muted(lines, state, &format!("  checkpoint {checkpoint}"));
    }
    if let Some(diagnostic) = attempt.diagnostic.as_ref() {
        text(
            lines,
            state,
            &format!("  {}: {}", diagnostic.code(), diagnostic.message()),
        );
    }
}

fn progress_lines(
    lines: &mut Vec<Line<'static>>,
    state: &TuiState,
    progress: &PlanAttemptProgressSnapshot,
) {
    text(
        lines,
        state,
        &format!(
            "- elapsed {}  turns {}  artifacts {}",
            duration(progress.elapsed_ms),
            progress.model_turns,
            progress.artifacts_created
        ),
    );
    if let Some(summary) = progress.summary.as_deref() {
        text(lines, state, &format!("  {summary}"));
    }
    if let Some(next) = progress.next_action.as_deref() {
        muted(lines, state, &format!("  next: {next}"));
    }
    let activity = match (
        progress.provider_request_in_flight,
        progress.tool_call_in_flight,
    ) {
        (true, true) => "provider and tool in flight",
        (true, false) => "provider request in flight",
        (false, true) => "tool call in flight",
        (false, false) => "idle at a safe boundary",
    };
    muted(lines, state, &format!("  {activity}"));
    if let Some(last) = progress.last_durable_progress_at_ms {
        muted(lines, state, &format!("  durable progress @ {last} ms"));
    }
}

fn directive_lines(
    lines: &mut Vec<Line<'static>>,
    state: &TuiState,
    directive: &CoordinatorDirectiveSnapshot,
) {
    text(
        lines,
        state,
        &format!(
            "- #{} {:?}  {:?}",
            directive.sequence, directive.kind, directive.status
        )
        .to_ascii_lowercase(),
    );
    text(lines, state, &format!("  {}", directive.reason));
    if let Some(instruction) = directive.instruction.as_deref() {
        muted(lines, state, &format!("  {instruction}"));
    }
}

fn visible_start(
    rows: &[PlanTreeRow],
    selected: Option<&merry_core::PlanNodeId>,
    requested: usize,
    capacity: usize,
) -> usize {
    if capacity == 0 || rows.is_empty() {
        return 0;
    }
    let total_height = rows.iter().map(physical_row_height).sum::<usize>();
    let max_start = (0..rows.len())
        .rev()
        .find(|&index| total_height.saturating_sub(physical_offset(rows, index)) >= capacity)
        .unwrap_or(0);
    let mut start = requested.min(max_start);
    if let Some(selected) = selected
        && let Some(index) = rows.iter().position(|row| &row.node_id == selected)
    {
        let selected_offset = physical_offset(rows, index);
        let start_offset = physical_offset(rows, start);
        if selected_offset < start_offset {
            start = index;
        } else if selected_offset >= start_offset + capacity {
            while start < index {
                let next_offset = physical_offset(rows, start + 1);
                start += 1;
                if selected_offset < next_offset + capacity {
                    break;
                }
            }
        }
    }
    start
}

fn physical_row_height(row: &PlanTreeRow) -> usize {
    1 + usize::from(row.activity.is_some())
}

fn physical_offset(rows: &[PlanTreeRow], index: usize) -> usize {
    rows.iter().take(index).map(physical_row_height).sum()
}

fn section(lines: &mut Vec<Line<'static>>, state: &TuiState, label: &str) {
    if !lines.is_empty() {
        lines.push(Line::default());
    }
    lines.push(Line::from(Span::styled(
        label.to_owned(),
        style(state, SemanticColor::ToolKeyword).add_modifier(Modifier::BOLD),
    )));
}

fn text(lines: &mut Vec<Line<'static>>, state: &TuiState, value: &str) {
    lines.push(Line::from(Span::styled(
        value.to_owned(),
        style(state, SemanticColor::Assistant),
    )));
}

fn muted(lines: &mut Vec<Line<'static>>, state: &TuiState, value: &str) {
    lines.push(Line::from(Span::styled(
        value.to_owned(),
        style(state, SemanticColor::Muted),
    )));
}

fn key_value(lines: &mut Vec<Line<'static>>, state: &TuiState, key: &str, value: &str) {
    lines.push(Line::from(vec![
        Span::styled(format!("{key}: "), style(state, SemanticColor::Muted)),
        Span::styled(value.to_owned(), style(state, SemanticColor::Assistant)),
    ]));
}

fn optional_number(
    lines: &mut Vec<Line<'static>>,
    state: &TuiState,
    key: &str,
    value: Option<u32>,
) {
    key_value(
        lines,
        state,
        key,
        &value.map_or_else(|| "inherited".to_owned(), |value| value.to_string()),
    );
}

fn named_values<'a>(
    lines: &mut Vec<Line<'static>>,
    state: &TuiState,
    key: &str,
    values: impl Iterator<Item = &'a str>,
) {
    let joined = values.collect::<Vec<_>>().join(", ");
    key_value(
        lines,
        state,
        key,
        if joined.is_empty() { "none" } else { &joined },
    );
}

fn list(lines: &mut Vec<Line<'static>>, state: &TuiState, values: &[String], empty: &str) {
    if values.is_empty() {
        muted(lines, state, empty);
    } else {
        for value in values {
            text(lines, state, &format!("- {value}"));
        }
    }
}

fn compact_id(value: &str, width: u16) -> String {
    let limit = usize::from(width.saturating_sub(18)).max(8);
    if value.chars().count() <= limit {
        value.to_owned()
    } else {
        format!(
            "{}...",
            value
                .chars()
                .take(limit.saturating_sub(3))
                .collect::<String>()
        )
    }
}

fn status_symbol(status: PlanNodeStatus, ready: bool) -> &'static str {
    if ready {
        return "◇";
    }
    match status {
        PlanNodeStatus::Pending => "o",
        PlanNodeStatus::InProgress | PlanNodeStatus::Verifying => "◆",
        PlanNodeStatus::Expanded => "◇",
        PlanNodeStatus::Completed => "✓",
        PlanNodeStatus::Blocked => "!",
        PlanNodeStatus::Failed => "x",
        PlanNodeStatus::Superseded => "~",
    }
}

fn status_color(status: PlanNodeStatus, ready: bool) -> SemanticColor {
    if ready {
        return SemanticColor::ToolKeyword;
    }
    match status {
        PlanNodeStatus::Pending | PlanNodeStatus::Superseded => SemanticColor::Muted,
        PlanNodeStatus::InProgress | PlanNodeStatus::Verifying | PlanNodeStatus::Expanded => {
            SemanticColor::Focus
        }
        PlanNodeStatus::Completed => SemanticColor::Success,
        PlanNodeStatus::Blocked => SemanticColor::Warning,
        PlanNodeStatus::Failed => SemanticColor::Error,
    }
}

fn activity_phase_style(state: &TuiState, phase: merry_core::SubagentActivityPhase) -> Style {
    let color = match phase {
        merry_core::SubagentActivityPhase::Starting
        | merry_core::SubagentActivityPhase::Waiting => SemanticColor::Warning,
        merry_core::SubagentActivityPhase::Running => SemanticColor::Focus,
        merry_core::SubagentActivityPhase::Completed => SemanticColor::Success,
        merry_core::SubagentActivityPhase::Failed => SemanticColor::Error,
        merry_core::SubagentActivityPhase::Cancelled => SemanticColor::Muted,
    };
    style(state, color)
}

fn activity_phase_label(phase: merry_core::SubagentActivityPhase) -> &'static str {
    match phase {
        merry_core::SubagentActivityPhase::Starting => "starting",
        merry_core::SubagentActivityPhase::Running => "running",
        merry_core::SubagentActivityPhase::Waiting => "waiting",
        merry_core::SubagentActivityPhase::Completed => "completed",
        merry_core::SubagentActivityPhase::Failed => "failed",
        merry_core::SubagentActivityPhase::Cancelled => "cancelled",
    }
}

fn bounded_activity_summary(summary: &str) -> String {
    const MAX_ACTIVITY_SUMMARY_CHARS: usize = 120;
    let mut chars = summary.chars();
    let bounded = chars
        .by_ref()
        .take(MAX_ACTIVITY_SUMMARY_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}...")
    } else {
        bounded
    }
}

fn phase_style(state: &TuiState, phase: PlanPhase) -> Style {
    let color = match phase {
        PlanPhase::Planning | PlanPhase::AwaitingApproval => SemanticColor::Warning,
        PlanPhase::Executing => SemanticColor::Focus,
        PlanPhase::Completed => SemanticColor::Success,
        PlanPhase::Blocked => SemanticColor::Error,
        PlanPhase::Cancelled => SemanticColor::Muted,
    };
    style(state, color)
}

fn phase_label(phase: PlanPhase) -> &'static str {
    match phase {
        PlanPhase::Planning => "planning",
        PlanPhase::AwaitingApproval => "awaiting approval",
        PlanPhase::Executing => "executing",
        PlanPhase::Completed => "completed",
        PlanPhase::Blocked => "blocked",
        PlanPhase::Cancelled => "cancelled",
    }
}

fn node_status_label(status: PlanNodeStatus) -> &'static str {
    match status {
        PlanNodeStatus::Pending => "pending",
        PlanNodeStatus::InProgress => "in progress",
        PlanNodeStatus::Expanded => "expanded",
        PlanNodeStatus::Verifying => "verifying",
        PlanNodeStatus::Completed => "completed",
        PlanNodeStatus::Blocked => "blocked",
        PlanNodeStatus::Failed => "failed",
        PlanNodeStatus::Superseded => "superseded",
    }
}

fn executor_label(executor: PlanExecutorPolicy) -> &'static str {
    match executor {
        PlanExecutorPolicy::Local => "local",
        PlanExecutorPolicy::Delegate => "subagent",
        PlanExecutorPolicy::Auto => "auto",
    }
}

fn approval_kind(kind: &PlanApprovalRequirementKind) -> &'static str {
    match kind {
        PlanApprovalRequirementKind::UserReviewRequested => "user review",
        PlanApprovalRequirementKind::SkillReviewRequested { .. } => "skill review",
        PlanApprovalRequirementKind::RootObjectiveChange => "root objective change",
        PlanApprovalRequirementKind::RootAcceptanceChange => "root acceptance change",
        PlanApprovalRequirementKind::CapabilityOrPermissionExpansion => "permission expansion",
        PlanApprovalRequirementKind::DestructiveExternalAuthority => "destructive authority",
        PlanApprovalRequirementKind::RequiredExternalInput { .. } => "external input",
    }
}

fn duration(milliseconds: u64) -> String {
    let seconds = milliseconds / 1_000;
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

fn selection_style(state: &TuiState) -> Style {
    let mut selected = style(state, SemanticColor::Assistant).add_modifier(Modifier::BOLD);
    if let Some(background) = state.theme().color(SemanticColor::Selection) {
        selected = selected.bg(background);
    }
    selected
}

fn style(state: &TuiState, color: SemanticColor) -> Style {
    state
        .theme()
        .color(color)
        .map_or_else(Style::default, |color| Style::default().fg(color))
}
