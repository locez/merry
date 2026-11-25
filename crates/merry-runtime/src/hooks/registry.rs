//! Composable in-process Rust hook registry with middleware chaining.

use super::{
    AfterModelDecision, AfterModelStepContext, AfterToolCallContext, AfterToolDecision,
    BeforeModelDecision, BeforeModelStepContext, BeforeToolCallContext, BeforeToolDecision,
    HookError, HookFuture, HookSubscriptions, RuntimeHookHandler, ToolHookFilter,
};
use merry_core::ToolName;
use merry_llm::GenerationConfig;
use std::{collections::BTreeSet, fmt, future::Future, sync::Arc};
use tokio_util::sync::CancellationToken;

type BeforeModelHookFn = Arc<
    dyn Fn(BeforeModelStepContext, CancellationToken) -> HookFuture<'static, BeforeModelDecision>
        + Send
        + Sync,
>;
type AfterModelHookFn = Arc<
    dyn Fn(AfterModelStepContext, CancellationToken) -> HookFuture<'static, AfterModelDecision>
        + Send
        + Sync,
>;
type BeforeToolHookFn = Arc<
    dyn Fn(BeforeToolCallContext, CancellationToken) -> HookFuture<'static, BeforeToolDecision>
        + Send
        + Sync,
>;
type AfterToolHookFn = Arc<
    dyn Fn(AfterToolCallContext, CancellationToken) -> HookFuture<'static, AfterToolDecision>
        + Send
        + Sync,
>;

#[derive(Clone)]
struct FilteredToolHook<F> {
    filter: ToolHookFilter,
    callback: F,
}

/// Composable in-process Rust hook registry with middleware chaining and short-circuiting.
#[derive(Clone, Default)]
pub struct HookRegistry {
    before_model: Vec<BeforeModelHookFn>,
    after_model: Vec<AfterModelHookFn>,
    before_tool: Vec<FilteredToolHook<BeforeToolHookFn>>,
    after_tool: Vec<FilteredToolHook<AfterToolHookFn>>,
}

impl fmt::Debug for HookRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HookRegistry")
            .field("before_model_count", &self.before_model.len())
            .field("after_model_count", &self.after_model.len())
            .field("before_tool_count", &self.before_tool.len())
            .field("after_tool_count", &self.after_tool.len())
            .finish()
    }
}

impl HookRegistry {
    /// Creates an empty hook registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns whether no hooks are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.before_model.is_empty()
            && self.after_model.is_empty()
            && self.before_tool.is_empty()
            && self.after_tool.is_empty()
    }

    /// Registers a `before_model_step` async hook.
    #[must_use]
    pub fn before_model_step<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(BeforeModelStepContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = BeforeModelDecision> + Send + 'static,
    {
        self.before_model.push(Arc::new(move |ctx, _token| {
            let fut = callback(ctx);
            Box::pin(async move { Ok(fut.await) })
        }));
        self
    }

    /// Registers an `after_model_step` async hook.
    #[must_use]
    pub fn after_model_step<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(AfterModelStepContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = AfterModelDecision> + Send + 'static,
    {
        self.after_model.push(Arc::new(move |ctx, _token| {
            let fut = callback(ctx);
            Box::pin(async move { Ok(fut.await) })
        }));
        self
    }

    /// Registers a `before_tool_call` async hook matching all tools.
    #[must_use]
    pub fn before_tool_call<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(BeforeToolCallContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = BeforeToolDecision> + Send + 'static,
    {
        self.before_tool.push(FilteredToolHook {
            filter: ToolHookFilter::All,
            callback: Arc::new(move |ctx, _token| {
                let fut = callback(ctx);
                Box::pin(async move { Ok(fut.await) })
            }),
        });
        self
    }

    /// Registers a `before_tool_call` async hook matching only `tools`.
    #[must_use]
    pub fn before_tool_call_for<I, F, Fut>(mut self, tools: I, callback: F) -> Self
    where
        I: IntoIterator<Item = ToolName>,
        F: Fn(BeforeToolCallContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = BeforeToolDecision> + Send + 'static,
    {
        let set: BTreeSet<ToolName> = tools.into_iter().collect();
        self.before_tool.push(FilteredToolHook {
            filter: if set.is_empty() {
                ToolHookFilter::None
            } else {
                ToolHookFilter::Only(set)
            },
            callback: Arc::new(move |ctx, _token| {
                let fut = callback(ctx);
                Box::pin(async move { Ok(fut.await) })
            }),
        });
        self
    }

    /// Registers an `after_tool_call` async hook matching all tools.
    #[must_use]
    pub fn after_tool_call<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(AfterToolCallContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = AfterToolDecision> + Send + 'static,
    {
        self.after_tool.push(FilteredToolHook {
            filter: ToolHookFilter::All,
            callback: Arc::new(move |ctx, _token| {
                let fut = callback(ctx);
                Box::pin(async move { Ok(fut.await) })
            }),
        });
        self
    }

    /// Registers an `after_tool_call` async hook matching only `tools`.
    #[must_use]
    pub fn after_tool_call_for<I, F, Fut>(mut self, tools: I, callback: F) -> Self
    where
        I: IntoIterator<Item = ToolName>,
        F: Fn(AfterToolCallContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = AfterToolDecision> + Send + 'static,
    {
        let set: BTreeSet<ToolName> = tools.into_iter().collect();
        self.after_tool.push(FilteredToolHook {
            filter: if set.is_empty() {
                ToolHookFilter::None
            } else {
                ToolHookFilter::Only(set)
            },
            callback: Arc::new(move |ctx, _token| {
                let fut = callback(ctx);
                Box::pin(async move { Ok(fut.await) })
            }),
        });
        self
    }
}

impl RuntimeHookHandler for HookRegistry {
    fn subscriptions(&self) -> HookSubscriptions {
        let mut before_tool = ToolHookFilter::None;
        for entry in &self.before_tool {
            before_tool.union_with(&entry.filter);
        }
        let mut after_tool = ToolHookFilter::None;
        for entry in &self.after_tool {
            after_tool.union_with(&entry.filter);
        }
        HookSubscriptions::none()
            .with_before_model_step(!self.before_model.is_empty())
            .with_after_model_step(!self.after_model.is_empty())
            .with_before_tool_call(before_tool)
            .with_after_tool_call(after_tool)
    }

    fn before_model_step<'a>(
        &'a self,
        context: BeforeModelStepContext,
        cancellation_token: CancellationToken,
    ) -> HookFuture<'a, BeforeModelDecision> {
        Box::pin(async move {
            let mut merged_generation: Option<GenerationConfig> = None;
            let mut notes: Vec<String> = Vec::new();
            let mut force_compact = false;
            let mut current = Some(context);
            let total = self.before_model.len();

            for (idx, hook) in self.before_model.iter().enumerate() {
                if cancellation_token.is_cancelled() {
                    return Err(HookError::Cancelled);
                }
                let is_last = idx + 1 == total;
                let Some(hook_ctx) = (if is_last {
                    current.take()
                } else {
                    current.clone()
                }) else {
                    break;
                };
                let decision = tokio::select! {
                    biased;
                    () = cancellation_token.cancelled() => return Err(HookError::Cancelled),
                    result = hook(hook_ctx, cancellation_token.clone()) => result?,
                };
                match decision {
                    BeforeModelDecision::Continue {
                        generation_override,
                        dynamic_steering_note,
                        force_compact: compact,
                    } => {
                        if let Some(generation_cfg) = generation_override {
                            if let Some(ctx) = current.as_mut() {
                                ctx.generation_config = generation_cfg.clone();
                            }
                            merged_generation = Some(generation_cfg);
                        }
                        if let Some(note) = dynamic_steering_note
                            && !note.trim().is_empty()
                        {
                            notes.push(note);
                        }
                        force_compact |= compact;
                    }
                    stop @ BeforeModelDecision::StopTurn { .. } => return Ok(stop),
                }
            }

            Ok(BeforeModelDecision::Continue {
                generation_override: merged_generation,
                dynamic_steering_note: (!notes.is_empty()).then(|| notes.join("\n\n")),
                force_compact,
            })
        })
    }

    fn after_model_step<'a>(
        &'a self,
        context: AfterModelStepContext,
        cancellation_token: CancellationToken,
    ) -> HookFuture<'a, AfterModelDecision> {
        Box::pin(async move {
            let mut current = Some(context);
            let total = self.after_model.len();

            for (idx, hook) in self.after_model.iter().enumerate() {
                if cancellation_token.is_cancelled() {
                    return Err(HookError::Cancelled);
                }
                let is_last = idx + 1 == total;
                let Some(hook_ctx) = (if is_last {
                    current.take()
                } else {
                    current.clone()
                }) else {
                    break;
                };
                let decision = tokio::select! {
                    biased;
                    () = cancellation_token.cancelled() => return Err(HookError::Cancelled),
                    result = hook(hook_ctx, cancellation_token.clone()) => result?,
                };
                match decision {
                    AfterModelDecision::Accept => {}
                    short_circuit => return Ok(short_circuit),
                }
            }
            Ok(AfterModelDecision::Accept)
        })
    }

    fn before_tool_call<'a>(
        &'a self,
        mut context: BeforeToolCallContext,
        cancellation_token: CancellationToken,
    ) -> HookFuture<'a, BeforeToolDecision> {
        Box::pin(async move {
            let matching: Vec<&BeforeToolHookFn> = self
                .before_tool
                .iter()
                .filter(|entry| entry.filter.matches(&context.tool_name))
                .map(|entry| &entry.callback)
                .collect();
            let mut modified = false;
            let total = matching.len();

            for (idx, hook) in matching.into_iter().enumerate() {
                if cancellation_token.is_cancelled() {
                    return Err(HookError::Cancelled);
                }
                let is_last = idx + 1 == total;
                if is_last {
                    let saved_args = modified.then(|| context.arguments.clone());
                    let decision = tokio::select! {
                        biased;
                        () = cancellation_token.cancelled() => return Err(HookError::Cancelled),
                        result = hook(context, cancellation_token.clone()) => result?,
                    };
                    return Ok(match decision {
                        BeforeToolDecision::Allow => match saved_args {
                            Some(arguments) => BeforeToolDecision::ModifyInput { arguments },
                            None => BeforeToolDecision::Allow,
                        },
                        other => other,
                    });
                }

                let decision = tokio::select! {
                    biased;
                    () = cancellation_token.cancelled() => return Err(HookError::Cancelled),
                    result = hook(context.clone(), cancellation_token.clone()) => result?,
                };
                match decision {
                    BeforeToolDecision::Allow => {}
                    BeforeToolDecision::ModifyInput { arguments } => {
                        context.arguments = arguments;
                        modified = true;
                    }
                    short_circuit => return Ok(short_circuit),
                }
            }
            Ok(BeforeToolDecision::Allow)
        })
    }

    fn after_tool_call<'a>(
        &'a self,
        mut context: AfterToolCallContext,
        cancellation_token: CancellationToken,
    ) -> HookFuture<'a, AfterToolDecision> {
        Box::pin(async move {
            let matching: Vec<&AfterToolHookFn> = self
                .after_tool
                .iter()
                .filter(|entry| entry.filter.matches(&context.tool_name))
                .map(|entry| &entry.callback)
                .collect();
            let mut modified = false;
            let total = matching.len();

            for (idx, hook) in matching.into_iter().enumerate() {
                if cancellation_token.is_cancelled() {
                    return Err(HookError::Cancelled);
                }
                let is_last = idx + 1 == total;
                if is_last {
                    let saved_output = modified.then(|| context.output.clone());
                    let decision = tokio::select! {
                        biased;
                        () = cancellation_token.cancelled() => return Err(HookError::Cancelled),
                        result = hook(context, cancellation_token.clone()) => result?,
                    };
                    return Ok(match decision {
                        AfterToolDecision::Continue => match saved_output {
                            Some(output) => AfterToolDecision::ModifyOutput { output },
                            None => AfterToolDecision::Continue,
                        },
                        other => other,
                    });
                }

                let decision = tokio::select! {
                    biased;
                    () = cancellation_token.cancelled() => return Err(HookError::Cancelled),
                    result = hook(context.clone(), cancellation_token.clone()) => result?,
                };
                match decision {
                    AfterToolDecision::Continue => {}
                    AfterToolDecision::ModifyOutput { output } => {
                        context.output = output;
                        modified = true;
                    }
                    stop @ AfterToolDecision::StopTurn { .. } => return Ok(stop),
                }
            }
            Ok(AfterToolDecision::Continue)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::HookDecision;

    #[test]
    fn hook_subscriptions_merge_combines_points_and_tool_filters() {
        let alpha = ToolName::new("alpha").expect("valid tool name");
        let beta = ToolName::new("beta").expect("valid tool name");
        let gamma = ToolName::new("gamma").expect("valid tool name");

        let mut left = HookSubscriptions::none()
            .with_before_model_step(true)
            .with_before_tool_call(ToolHookFilter::Only(BTreeSet::from([alpha.clone()])));
        let right = HookSubscriptions::none()
            .with_after_model_step(true)
            .with_before_tool_call(ToolHookFilter::Only(BTreeSet::from([beta.clone()])))
            .with_after_tool_call(ToolHookFilter::All);

        left.merge_with(&right);

        assert!(left.matches_before_model_step());
        assert!(left.matches_after_model_step());
        assert!(left.matches_before_tool_call(&alpha));
        assert!(left.matches_before_tool_call(&beta));
        assert!(!left.matches_before_tool_call(&gamma));
        assert!(left.matches_after_tool_call(&gamma));
    }

    #[test]
    fn hook_decision_validation_rejects_blank_required_fields() {
        assert!(
            HookDecision::BeforeModelStep(BeforeModelDecision::stop_turn("   ", None))
                .validate()
                .is_err()
        );
        assert!(
            HookDecision::AfterModelStep(AfterModelDecision::continue_with(" \t "))
                .validate()
                .is_err()
        );
        assert!(
            HookDecision::BeforeToolCall(BeforeToolDecision::deny(""))
                .validate()
                .is_err()
        );
        assert!(
            HookDecision::AfterToolCall(AfterToolDecision::stop_turn("bad\0reason", None))
                .validate()
                .is_err()
        );
        assert!(
            HookDecision::AfterModelStep(AfterModelDecision::continue_with("refine answer"))
                .validate()
                .is_ok()
        );
    }
}
