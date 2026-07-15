use merry_core::{
    PlanApprovalRequirementKind, PlanApprovalRequirementStatus, PlanAttemptProgressSnapshot,
    PlanAttemptSnapshot, PlanCapabilityEnvelopeSnapshot, PlanExecutorPolicy, PlanLeaseSnapshot,
    PlanLinkStatus, PlanNodeId, PlanNodeSnapshot, PlanNodeStatus, PlanSnapshot,
    SubagentActivitySnapshot, SubagentId, SubagentTaskId,
};
use merry_runtime::PlanApprovalInput;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PlanCounts {
    pub(crate) live: usize,
    pub(crate) ready: usize,
    pub(crate) blocked: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanTreeRow {
    pub(crate) node_id: PlanNodeId,
    pub(crate) depth: usize,
    pub(crate) has_children: bool,
    pub(crate) collapsed: bool,
    pub(crate) ready: bool,
    pub(crate) status: PlanNodeStatus,
    pub(crate) executor_policy: PlanExecutorPolicy,
    pub(crate) objective: String,
    pub(crate) activity: Option<SubagentActivitySnapshot>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PlanUiState {
    snapshot: Option<PlanSnapshot>,
    selected_node_id: Option<PlanNodeId>,
    collapsed_node_ids: BTreeSet<PlanNodeId>,
    scroll_offset: usize,
    inspector_scroll_offset: usize,
    subagent_activity: BTreeMap<SubagentId, BTreeMap<SubagentTaskId, SubagentActivitySnapshot>>,
    open: bool,
    focused: bool,
    inspector_open: bool,
}

impl PlanUiState {
    pub(crate) fn snapshot(&self) -> Option<&PlanSnapshot> {
        self.snapshot.as_ref()
    }

    pub(crate) fn update_snapshot(&mut self, snapshot: PlanSnapshot) {
        let first_snapshot = self.snapshot.is_none();
        let known_ids = snapshot
            .nodes
            .iter()
            .filter(|node| node.status != PlanNodeStatus::Superseded)
            .map(|node| node.id.clone())
            .collect::<BTreeSet<_>>();
        self.collapsed_node_ids
            .retain(|node_id| known_ids.contains(node_id));
        if self
            .selected_node_id
            .as_ref()
            .is_none_or(|node_id| !known_ids.contains(node_id))
        {
            self.selected_node_id = snapshot
                .root_node_id
                .clone()
                .filter(|node_id| known_ids.contains(node_id))
                .or_else(|| first_root_id(&snapshot));
            self.inspector_scroll_offset = 0;
        }
        reveal_active_paths(&snapshot, &mut self.collapsed_node_ids);
        self.snapshot = Some(snapshot);
        if first_snapshot {
            self.open = true;
        }
        self.clamp_scroll_offset();
    }

    pub(crate) fn update_subagent_activity(&mut self, snapshots: Vec<SubagentActivitySnapshot>) {
        self.subagent_activity =
            snapshots
                .into_iter()
                .fold(BTreeMap::new(), |mut activity, snapshot| {
                    let subagent_id = snapshot.subagent_id.clone();
                    let task_id = snapshot.task_id.clone();
                    let tasks = activity.entry(subagent_id).or_default();
                    if tasks
                        .get(&task_id)
                        .is_none_or(|existing| existing.updated_at_ms <= snapshot.updated_at_ms)
                    {
                        tasks.insert(task_id, snapshot);
                    }
                    activity
                });
    }

    pub(crate) fn update_progress(&mut self, progress: PlanAttemptProgressSnapshot) {
        let Some(snapshot) = self.snapshot.as_mut() else {
            return;
        };
        if let Some(existing) = snapshot
            .attempt_progress
            .iter_mut()
            .find(|existing| existing.attempt_id == progress.attempt_id)
        {
            *existing = progress.clone();
        } else {
            snapshot.attempt_progress.push(progress.clone());
        }
        if let Some(heartbeat_at_ms) = progress.last_subagent_heartbeat_at_ms
            && let Some(lease) = snapshot
                .leases
                .iter_mut()
                .find(|lease| lease.attempt_id == progress.attempt_id)
        {
            lease.last_heartbeat_at_ms = heartbeat_at_ms;
            lease.lease_expires_at_ms = heartbeat_at_ms
                .saturating_add(snapshot.resource_policy_snapshot.subagent_heartbeat_ttl_ms);
        }
    }

    pub(crate) fn update_lease(&mut self, lease: PlanLeaseSnapshot) {
        let Some(snapshot) = self.snapshot.as_mut() else {
            return;
        };
        if let Some(existing) = snapshot
            .leases
            .iter_mut()
            .find(|existing| existing.lease_id == lease.lease_id)
        {
            *existing = lease;
        } else {
            snapshot.leases.push(lease);
        }
    }

    pub(crate) fn update_attempt(&mut self, attempt: PlanAttemptSnapshot) {
        let Some(snapshot) = self.snapshot.as_mut() else {
            return;
        };
        if let Some(existing) = snapshot
            .attempts
            .iter_mut()
            .find(|existing| existing.attempt_id == attempt.attempt_id)
        {
            *existing = attempt;
        } else {
            snapshot.attempts.push(attempt);
        }
    }

    pub(crate) fn selected_node_id(&self) -> Option<&PlanNodeId> {
        self.selected_node_id.as_ref()
    }

    pub(crate) fn selected_node(&self) -> Option<&PlanNodeSnapshot> {
        let selected = self.selected_node_id.as_ref()?;
        self.snapshot
            .as_ref()?
            .nodes
            .iter()
            .find(|node| &node.id == selected && node.status != PlanNodeStatus::Superseded)
    }

    pub(crate) fn activity_for_node(
        &self,
        node: &PlanNodeSnapshot,
    ) -> Option<&SubagentActivitySnapshot> {
        node.links
            .iter()
            .filter(|link| {
                link.status != PlanLinkStatus::Superseded && link.superseded_by.is_none()
            })
            .filter_map(|link| {
                self.subagent_activity
                    .get(&link.subagent_id)
                    .and_then(|tasks| tasks.get(&link.task_id))
            })
            .max_by_key(|activity| activity.updated_at_ms)
    }

    pub(crate) fn select_node(&mut self, node_id: PlanNodeId) -> bool {
        if !self.snapshot.as_ref().is_some_and(|snapshot| {
            snapshot
                .nodes
                .iter()
                .any(|node| node.id == node_id && node.status != PlanNodeStatus::Superseded)
        }) {
            return false;
        }
        self.selected_node_id = Some(node_id);
        self.inspector_scroll_offset = 0;
        true
    }

    pub(crate) fn is_collapsed(&self, node_id: &PlanNodeId) -> bool {
        self.collapsed_node_ids.contains(node_id)
    }

    pub(crate) fn toggle_collapse(&mut self, node_id: &PlanNodeId) -> bool {
        if !self.has_children(node_id) {
            return false;
        }
        if !self.collapsed_node_ids.remove(node_id) {
            self.collapsed_node_ids.insert(node_id.clone());
        }
        self.clamp_scroll_offset();
        true
    }

    pub(crate) fn visible_rows(&self) -> Vec<PlanTreeRow> {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Vec::new();
        };
        let mut rows = Vec::with_capacity(snapshot.nodes.len());
        let roots = root_nodes(snapshot);
        for root in roots {
            self.append_visible_rows(snapshot, root, 0, &mut rows);
        }
        rows
    }

    pub(crate) fn counts(&self) -> PlanCounts {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return PlanCounts::default();
        };
        PlanCounts {
            live: snapshot
                .nodes
                .iter()
                .map(|node| node.execution_summary.active as usize)
                .sum(),
            ready: snapshot
                .nodes
                .iter()
                .filter(|node| node_is_ready(snapshot, node))
                .count(),
            blocked: snapshot
                .nodes
                .iter()
                .filter(|node| node.status == PlanNodeStatus::Blocked)
                .count(),
        }
    }

    pub(crate) fn approval_input(&self) -> Result<PlanApprovalInput, String> {
        let snapshot = self
            .snapshot
            .as_ref()
            .ok_or_else(|| "no active plan is available".to_owned())?;
        let material = approval_material(snapshot);
        Ok(PlanApprovalInput {
            plan_id: snapshot.plan_id.clone(),
            expected_plan_revision: snapshot.revision,
            review_resolution_ref: "tui:user-approval".to_owned(),
            capability_envelope: Some(material.envelope),
            authorization_refs: vec!["tui:user-approval".to_owned()],
            requirement_resolution_refs: material.requirement_resolution_refs,
        })
    }

    pub(crate) fn approval_summary(&self) -> Result<String, String> {
        let snapshot = self
            .snapshot
            .as_ref()
            .ok_or_else(|| "no active plan is available".to_owned())?;
        let material = approval_material(snapshot);
        let mut lines = vec![
            format!("Plan revision {}", snapshot.revision),
            format!(
                "Tools: {}",
                display_values(
                    material
                        .envelope
                        .allowed_tools
                        .iter()
                        .map(|tool| tool.as_str())
                )
            ),
            format!(
                "Read scope: {}",
                display_values(material.envelope.read_scope.iter().map(String::as_str))
            ),
            format!(
                "Write scope: {}",
                display_values(material.envelope.write_scope.iter().map(String::as_str))
            ),
            format!(
                "Forbidden paths: {}",
                display_values(material.envelope.forbidden_paths.iter().map(String::as_str),)
            ),
            format!(
                "Destructive external authority: {}",
                if material.envelope.destructive_external_authority {
                    "requested"
                } else {
                    "not requested"
                }
            ),
        ];
        let pending = snapshot
            .approval_requirements
            .iter()
            .filter(|requirement| requirement.status == PlanApprovalRequirementStatus::Pending)
            .collect::<Vec<_>>();
        lines.push("Pending requirements:".to_owned());
        if pending.is_empty() {
            lines.push("- review boundary".to_owned());
        } else {
            lines.extend(
                pending.into_iter().map(|requirement| {
                    format!("- {}", approval_requirement_label(&requirement.kind))
                }),
            );
        }
        Ok(lines.join("\n"))
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn is_focused(&self) -> bool {
        self.open && self.focused
    }

    pub(crate) fn is_inspector_open(&self) -> bool {
        self.is_focused() && self.inspector_open
    }

    pub(crate) fn open_and_focus(&mut self) -> bool {
        if self.snapshot.is_none() {
            return false;
        }
        self.open = true;
        self.focused = true;
        true
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
        self.focused = false;
        self.inspector_open = false;
        self.inspector_scroll_offset = 0;
    }

    pub(crate) fn leave_focus(&mut self) {
        self.focused = false;
        self.inspector_open = false;
        self.inspector_scroll_offset = 0;
    }

    pub(crate) fn open_inspector(&mut self) -> bool {
        if self.selected_node().is_none() {
            return false;
        }
        self.inspector_open = true;
        self.inspector_scroll_offset = 0;
        true
    }

    pub(crate) fn close_inspector(&mut self) -> bool {
        if !self.inspector_open {
            return false;
        }
        self.inspector_open = false;
        self.inspector_scroll_offset = 0;
        true
    }

    pub(crate) fn select_previous(&mut self) -> bool {
        self.move_selection(-1)
    }

    pub(crate) fn select_next(&mut self) -> bool {
        self.move_selection(1)
    }

    pub(crate) fn select_parent_or_collapse(&mut self) -> bool {
        let Some(selected) = self.selected_node_id.clone() else {
            return false;
        };
        if self.has_children(&selected) && !self.is_collapsed(&selected) {
            return self.toggle_collapse(&selected);
        }
        let parent = self.selected_node().and_then(|node| node.parent_id.clone());
        parent.is_some_and(|parent| self.select_node(parent))
    }

    pub(crate) fn select_child_or_expand(&mut self) -> bool {
        let Some(selected) = self.selected_node_id.clone() else {
            return false;
        };
        if self.is_collapsed(&selected) {
            return self.toggle_collapse(&selected);
        }
        let child = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| children_of(snapshot, Some(&selected)).into_iter().next())
            .map(|node| node.id.clone());
        child.is_some_and(|child| self.select_node(child))
    }

    pub(crate) fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    pub(crate) fn scroll_up_by(&mut self, amount: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
    }

    pub(crate) fn scroll_down_by(&mut self, amount: usize) {
        self.scroll_offset = self
            .scroll_offset
            .saturating_add(amount)
            .min(self.visible_rows().len().saturating_sub(1));
    }

    pub(crate) fn inspector_scroll_offset(&self) -> usize {
        self.inspector_scroll_offset
    }

    pub(crate) fn scroll_inspector_up_by(&mut self, amount: usize) {
        self.inspector_scroll_offset = self.inspector_scroll_offset.saturating_sub(amount);
    }

    pub(crate) fn scroll_inspector_down_by(&mut self, amount: usize) {
        self.inspector_scroll_offset = self.inspector_scroll_offset.saturating_add(amount);
    }

    fn append_visible_rows(
        &self,
        snapshot: &PlanSnapshot,
        node: &PlanNodeSnapshot,
        depth: usize,
        rows: &mut Vec<PlanTreeRow>,
    ) {
        let children = children_of(snapshot, Some(&node.id));
        let collapsed = self.collapsed_node_ids.contains(&node.id);
        rows.push(PlanTreeRow {
            node_id: node.id.clone(),
            depth,
            has_children: !children.is_empty(),
            collapsed,
            ready: node_is_ready(snapshot, node),
            status: node.status,
            executor_policy: node.executor_policy,
            objective: node.objective.clone(),
            activity: self.activity_for_node(node).cloned(),
        });
        if collapsed {
            return;
        }
        for child in children {
            self.append_visible_rows(snapshot, child, depth + 1, rows);
        }
    }

    fn has_children(&self, node_id: &PlanNodeId) -> bool {
        self.snapshot.as_ref().is_some_and(|snapshot| {
            snapshot.nodes.iter().any(|node| {
                node.parent_id.as_ref() == Some(node_id)
                    && node.status != PlanNodeStatus::Superseded
            })
        })
    }

    fn move_selection(&mut self, direction: isize) -> bool {
        let rows = self.visible_rows();
        if rows.is_empty() {
            return false;
        }
        let current = self
            .selected_node_id
            .as_ref()
            .and_then(|selected| rows.iter().position(|row| &row.node_id == selected))
            .unwrap_or(0);
        let next = current
            .saturating_add_signed(direction)
            .min(rows.len().saturating_sub(1));
        if next == current {
            return false;
        }
        self.selected_node_id = Some(rows[next].node_id.clone());
        self.inspector_scroll_offset = 0;
        true
    }

    fn clamp_scroll_offset(&mut self) {
        self.scroll_offset = self
            .scroll_offset
            .min(self.visible_rows().len().saturating_sub(1));
    }
}

struct ApprovalMaterial {
    envelope: PlanCapabilityEnvelopeSnapshot,
    requirement_resolution_refs: BTreeMap<merry_core::PlanApprovalRequirementId, String>,
}

fn approval_material(snapshot: &PlanSnapshot) -> ApprovalMaterial {
    let mut tools = BTreeSet::new();
    let mut read_scope = BTreeSet::new();
    let mut write_scope = BTreeSet::new();
    let mut forbidden_paths = BTreeSet::new();
    let mut destructive_external_authority = false;
    if let Some(existing) = snapshot.authorized_capability_envelope.as_ref() {
        tools.extend(existing.allowed_tools.iter().cloned());
        read_scope.extend(existing.read_scope.iter().cloned());
        write_scope.extend(existing.write_scope.iter().cloned());
        forbidden_paths.extend(existing.forbidden_paths.iter().cloned());
        destructive_external_authority = existing.destructive_external_authority;
    }
    if let Some(root) = snapshot.root_node_id.as_ref().and_then(|root_id| {
        snapshot
            .nodes
            .iter()
            .find(|node| &node.id == root_id && node.status != PlanNodeStatus::Superseded)
    }) {
        tools.extend(root.harness.allowed_tools.iter().cloned());
        read_scope.extend(root.harness.read_scope.iter().cloned());
        write_scope.extend(root.harness.write_scope.iter().cloned());
        forbidden_paths.extend(root.harness.forbidden_paths.iter().cloned());
    }
    let requirement_resolution_refs = snapshot
        .approval_requirements
        .iter()
        .filter(|requirement| requirement.status == PlanApprovalRequirementStatus::Pending)
        .map(|requirement| {
            if matches!(
                &requirement.kind,
                PlanApprovalRequirementKind::DestructiveExternalAuthority
            ) {
                destructive_external_authority = true;
            }
            (
                requirement.requirement_id.clone(),
                "tui:user-approval".to_owned(),
            )
        })
        .collect();
    ApprovalMaterial {
        envelope: PlanCapabilityEnvelopeSnapshot {
            allowed_tools: tools.into_iter().collect(),
            read_scope: read_scope.into_iter().collect(),
            write_scope: write_scope.into_iter().collect(),
            forbidden_paths: forbidden_paths.into_iter().collect(),
            destructive_external_authority,
        },
        requirement_resolution_refs,
    }
}

fn approval_requirement_label(kind: &PlanApprovalRequirementKind) -> String {
    match kind {
        PlanApprovalRequirementKind::UserReviewRequested => "user review".to_owned(),
        PlanApprovalRequirementKind::SkillReviewRequested { skill_id } => {
            format!("skill review ({})", skill_id.as_str())
        }
        PlanApprovalRequirementKind::RootObjectiveChange => "root objective change".to_owned(),
        PlanApprovalRequirementKind::RootAcceptanceChange => "root acceptance change".to_owned(),
        PlanApprovalRequirementKind::CapabilityOrPermissionExpansion => {
            "capability or permission expansion".to_owned()
        }
        PlanApprovalRequirementKind::DestructiveExternalAuthority => {
            "destructive external authority".to_owned()
        }
        PlanApprovalRequirementKind::RequiredExternalInput { prompt } => {
            format!("external input: {prompt}")
        }
    }
}

fn display_values<'a>(values: impl Iterator<Item = &'a str>) -> String {
    let values = values.collect::<Vec<_>>();
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(", ")
    }
}

fn reveal_active_paths(snapshot: &PlanSnapshot, collapsed: &mut BTreeSet<PlanNodeId>) {
    let mut active = snapshot
        .nodes
        .iter()
        .filter(|node| {
            matches!(
                node.status,
                PlanNodeStatus::InProgress | PlanNodeStatus::Verifying
            )
        })
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    active.extend(
        snapshot
            .leases
            .iter()
            .filter(|lease| lease.status == merry_core::PlanLeaseStatus::Live)
            .map(|lease| lease.node_id.clone()),
    );
    active.extend(
        snapshot
            .nodes
            .iter()
            .filter(|node| {
                node.execution_summary.active > 0
                    || node
                        .links
                        .iter()
                        .any(|link| link.status == PlanLinkStatus::Active)
            })
            .map(|node| node.id.clone()),
    );
    for node_id in active {
        let mut parent = node(snapshot, &node_id).and_then(|node| node.parent_id.as_ref());
        while let Some(parent_id) = parent {
            collapsed.remove(parent_id);
            parent = node(snapshot, parent_id).and_then(|node| node.parent_id.as_ref());
        }
    }
}

fn node<'a>(snapshot: &'a PlanSnapshot, node_id: &PlanNodeId) -> Option<&'a PlanNodeSnapshot> {
    snapshot.nodes.iter().find(|node| &node.id == node_id)
}

fn first_root_id(snapshot: &PlanSnapshot) -> Option<PlanNodeId> {
    root_nodes(snapshot).first().map(|node| node.id.clone())
}

fn root_nodes(snapshot: &PlanSnapshot) -> Vec<&PlanNodeSnapshot> {
    let mut roots = children_of(snapshot, None);
    if let Some(root_id) = snapshot.root_node_id.as_ref()
        && let Some(position) = roots.iter().position(|node| &node.id == root_id)
    {
        roots.swap(0, position);
    }
    roots
}

fn children_of<'a>(
    snapshot: &'a PlanSnapshot,
    parent_id: Option<&PlanNodeId>,
) -> Vec<&'a PlanNodeSnapshot> {
    let mut children = snapshot
        .nodes
        .iter()
        .filter(|node| {
            node.parent_id.as_ref() == parent_id && node.status != PlanNodeStatus::Superseded
        })
        .collect::<Vec<_>>();
    children.sort_by(|left, right| {
        left.sibling_order
            .cmp(&right.sibling_order)
            .then_with(|| left.id.cmp(&right.id))
    });
    children
}

fn node_is_ready(snapshot: &PlanSnapshot, node: &PlanNodeSnapshot) -> bool {
    if node.status == PlanNodeStatus::Superseded || node.execution_summary.active > 0 {
        return false;
    }
    let dependencies_completed = node.depends_on.iter().all(|dependency| {
        snapshot.nodes.iter().any(|candidate| {
            candidate.id == *dependency && candidate.status == PlanNodeStatus::Completed
        })
    });
    if !dependencies_completed {
        return false;
    }
    let children = snapshot
        .nodes
        .iter()
        .filter(|candidate| {
            candidate.parent_id.as_ref() == Some(&node.id)
                && candidate.status != PlanNodeStatus::Superseded
        })
        .collect::<Vec<_>>();
    let shape_ready = if children.is_empty() {
        node.status == PlanNodeStatus::Pending
    } else {
        node.status == PlanNodeStatus::Verifying
            && children
                .iter()
                .all(|child| child.status == PlanNodeStatus::Completed)
    };
    if !shape_ready {
        return false;
    }
    true
}
