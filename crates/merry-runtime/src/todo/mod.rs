//! Lightweight session-scoped checklist state and `update_todos` intrinsic tool.

use crate::{
    RegisteredTool, ToolActionKind, ToolExecutionContext, ToolExecutionError, ToolExecutor,
    ToolExecutorFuture,
};
use merry_core::{
    CoreError, MAX_TODO_CONTENT_BYTES, MAX_TODO_ID_BYTES, MAX_TODO_ITEMS, TodoItem, TodoStatus,
    ToolName, ToolSpec,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fmt::Write, sync::Arc};
use thiserror::Error;

pub(crate) const UPDATE_TODOS_TOOL_NAME: &str = "update_todos";

/// Errors produced when validating a checklist update.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum TodoError {
    /// Two entries in the same update reused the same id.
    #[error("duplicate todo id in request: \"{id}\"")]
    DuplicateId { id: String },
    /// An item id was empty, too long, or contained control characters.
    #[error("todo id is invalid: {reason}")]
    InvalidId { reason: &'static str },
    /// Item content exceeded the bounded size limit.
    #[error("todo content for \"{id}\" exceeds maximum length of {maximum} bytes")]
    ContentTooLong { id: String, maximum: usize },
    /// Resulting checklist exceeded the maximum item count.
    #[error("todo list exceeds maximum of {maximum} items")]
    TooManyItems { maximum: usize },
}

/// One item patch passed to `update_todos`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TodoUpdateItem {
    /// Stable identifier for this checklist item (e.g. `"1"`, `"gate"`, `"tui"`).
    #[schemars(description = "Unique identifier for the checklist item.")]
    pub(crate) id: String,
    /// Optional task description. Omit when updating status on an existing item.
    #[serde(default)]
    #[schemars(
        description = "Task description. Optional when merging status updates onto an existing item."
    )]
    pub(crate) content: Option<String>,
    /// Optional status (`pending`, `in_progress`, `completed`, `cancelled`).
    #[serde(default)]
    #[schemars(
        description = "Item status: pending, in_progress, completed, or cancelled. Defaults to pending for new items."
    )]
    pub(crate) status: Option<TodoStatus>,
}

impl TodoUpdateItem {
    fn has_no_content(&self) -> bool {
        self.content
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
    }
}

const fn default_merge() -> bool {
    true
}

/// Input for `update_todos`.
#[merry_tools_macros::tool(
    crate = "crate",
    name = "update_todos",
    description = "Create or update the live execution checklist shown to the user."
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateTodosInput {
    /// When true (default), merges updates by id so flipping status only requires `{id, status}`.
    /// When false, replaces the entire checklist.
    #[serde(default = "default_merge")]
    #[schemars(
        description = "Optional. When true (default), merges updates into the existing list by id (send only `{id, status}` to flip status). When false, replaces the entire list."
    )]
    pub(crate) merge: bool,
    /// Checklist items to create, update, or replace.
    #[schemars(description = "Ordered checklist items to apply.")]
    pub(crate) todos: Vec<TodoUpdateItem>,
}

/// Runtime-owned flat checklist state for one session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TodoListState {
    items: Vec<TodoItem>,
}

impl TodoListState {
    /// Restores checklist state from persisted items.
    #[must_use]
    pub(crate) fn from_items(items: Vec<TodoItem>) -> Self {
        Self { items }
    }

    /// Returns the ordered checklist items.
    #[must_use]
    pub(crate) fn items(&self) -> &[TodoItem] {
        &self.items
    }

    /// Applies one `update_todos` request with forgiving merge semantics.
    pub(crate) fn apply(&mut self, input: UpdateTodosInput) -> Result<String, TodoError> {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for item in &input.todos {
            let id = item.id.trim();
            if id.is_empty() || id.len() > MAX_TODO_ID_BYTES || id.chars().any(char::is_control) {
                return Err(TodoError::InvalidId {
                    reason: "must be non-blank, <= 128 bytes, without control characters",
                });
            }
            if !seen.insert(id) {
                return Err(TodoError::DuplicateId { id: id.to_owned() });
            }
            if let Some(content) = item.content.as_deref()
                && content.trim().len() > MAX_TODO_CONTENT_BYTES
            {
                return Err(TodoError::ContentTooLong {
                    id: id.to_owned(),
                    maximum: MAX_TODO_CONTENT_BYTES,
                });
            }
        }

        // Auto-upgrade to merge when the model forgot `merge: true` on a
        // status-only patch targeting existing ids.
        let effective_merge = input.merge
            || (!self.items.is_empty()
                && !input.todos.is_empty()
                && input.todos.iter().all(|update| {
                    update.has_no_content()
                        && self
                            .items
                            .iter()
                            .any(|existing| existing.id == update.id.trim())
                }));

        let mut next = if effective_merge {
            self.items.clone()
        } else {
            Vec::with_capacity(input.todos.len())
        };

        for update in input.todos {
            let id = update.id.trim().to_owned();
            let content_opt = update
                .content
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            if let Some(existing) = next.iter_mut().find(|item| item.id == id) {
                if let Some(content) = content_opt {
                    existing.content = content;
                }
                if let Some(status) = update.status {
                    existing.status = status;
                }
            } else {
                let content = content_opt.unwrap_or_else(|| id.clone());
                let status = update.status.unwrap_or(TodoStatus::Pending);
                next.push(TodoItem {
                    id,
                    content,
                    status,
                });
            }
        }

        if next.len() > MAX_TODO_ITEMS {
            return Err(TodoError::TooManyItems {
                maximum: MAX_TODO_ITEMS,
            });
        }

        self.items = next;
        Ok(self.summarize())
    }

    /// Formats a compact human/model-readable checklist summary.
    #[must_use]
    fn summarize(&self) -> String {
        if self.items.is_empty() {
            return "No tasks currently tracked.".to_owned();
        }
        let mut out = String::new();
        for item in &self.items {
            let _ = writeln!(
                &mut out,
                "- {} {}: {}",
                item.status.tag(),
                item.id,
                item.content
            );
        }
        out
    }

    /// Dynamic prompt projection injected only when active/open todos exist.
    #[must_use]
    pub(crate) fn dynamic_prompt_context(&self) -> Option<String> {
        let has_open = self
            .items
            .iter()
            .any(|item| matches!(item.status, TodoStatus::Pending | TodoStatus::InProgress));
        if !has_open {
            return None;
        }
        Some(format!("<todo_list>\n{}</todo_list>", self.summarize()))
    }
}

pub(crate) fn todo_registered_tool() -> Result<RegisteredTool, CoreError> {
    let spec: ToolSpec = UpdateTodosInput::tool_spec_with(
        UPDATE_TODOS_TOOL_NAME,
        "Create and manage a lightweight execution checklist shown live to the user. Use for multi-step work (3+ steps); skip for trivial single-step tasks. Default `merge: true` lets you update status by sending only `{id, status}`. Mark an item `completed` only after verifying the actual change or check.",
    )
    .map_err(CoreError::from)?;
    Ok(RegisteredTool::new(
        spec,
        Arc::new(IntrinsicTodoExecutor),
        ToolActionKind::RuntimeControl,
    ))
}

pub(crate) fn is_todo_tool(name: &ToolName) -> bool {
    name.as_str() == UPDATE_TODOS_TOOL_NAME
}

struct IntrinsicTodoExecutor;

impl ToolExecutor for IntrinsicTodoExecutor {
    fn execute<'a>(
        &'a self,
        call: merry_core::PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            Err(ToolExecutionError::infrastructure(format!(
                "intrinsic todo tool {} reached the generic executor",
                call.name()
            )))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, content: Option<&str>, status: Option<TodoStatus>) -> TodoUpdateItem {
        TodoUpdateItem {
            id: id.to_owned(),
            content: content.map(str::to_owned),
            status,
        }
    }

    #[test]
    fn merge_updates_status_without_requiring_content() {
        let mut list = TodoListState::default();
        list.apply(UpdateTodosInput {
            merge: false,
            todos: vec![
                item("1", Some("Build gate"), Some(TodoStatus::InProgress)),
                item("2", Some("Add modal"), Some(TodoStatus::Pending)),
            ],
        })
        .expect("seed");

        let summary = list
            .apply(UpdateTodosInput {
                merge: true,
                todos: vec![
                    item("1", None, Some(TodoStatus::Completed)),
                    item("2", None, Some(TodoStatus::InProgress)),
                ],
            })
            .expect("merge");

        assert!(summary.contains("[completed] 1: Build gate"));
        assert!(summary.contains("[in_progress] 2: Add modal"));
    }

    #[test]
    fn auto_upgrades_replace_to_merge_for_status_only_patch() {
        let mut list = TodoListState::default();
        list.apply(UpdateTodosInput {
            merge: false,
            todos: vec![
                item("1", Some("Step A"), Some(TodoStatus::InProgress)),
                item("2", Some("Step B"), Some(TodoStatus::Pending)),
            ],
        })
        .expect("seed");

        list.apply(UpdateTodosInput {
            merge: false,
            todos: vec![item("1", None, Some(TodoStatus::Completed))],
        })
        .expect("auto-merge");

        assert_eq!(list.items().len(), 2);
        assert_eq!(list.items()[0].status, TodoStatus::Completed);
        assert_eq!(list.items()[1].content, "Step B");
    }

    #[test]
    fn rejects_duplicate_ids_and_hides_dynamic_context_when_all_done() {
        let mut list = TodoListState::default();
        let err = list
            .apply(UpdateTodosInput {
                merge: true,
                todos: vec![item("dup", Some("A"), None), item("dup", Some("B"), None)],
            })
            .expect_err("duplicate rejected");
        assert!(matches!(err, TodoError::DuplicateId { .. }));

        list.apply(UpdateTodosInput {
            merge: false,
            todos: vec![item("1", Some("Done"), Some(TodoStatus::Completed))],
        })
        .expect("completed");
        assert_eq!(list.dynamic_prompt_context(), None);
    }
}
