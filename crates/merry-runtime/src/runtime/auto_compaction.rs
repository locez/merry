use super::{RuntimeInner, provider_request::resolve_request_context_window};
use crate::{
    CitationCompactionInput, CitationCompactionPolicy, CompactionError, CompactionOutcome,
    ResolvedCitationCompactionBudget, ResolvedContextWindow, RuntimeError, RuntimeModelRole,
    compaction::{
        ArchiveOnlyCompactionInput, CompactionCoverageBudget, CompactionPreparation,
        CompactionReasoningReserve, CompactionWindowBudget, compaction_model_window,
        compaction_request_required_tokens, compaction_window_safety_tokens,
        compile_citation_compaction_model_request, generate_validated_compaction_candidate,
        validate_compaction_model_window,
    },
    events::ActiveStepPermit,
    session::{PreparedCompactionInstall, SessionState},
    session_store::StagedSessionBundle,
    step::{StablePrefixParts, compile_stable_prefix_items},
};
use merry_llm::{ModelInputItem, ModelStreamContext, ReasoningEffort};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(super) async fn compaction_preparation_for_hard_watermark(
    inner: &RuntimeInner,
    policy: CitationCompactionPolicy,
    resolved_budget: ResolvedCitationCompactionBudget,
    window_budget: CompactionWindowBudget,
    primary_window_tokens: u64,
) -> Result<Option<(CompactionPreparation, CompactionRequestBudget)>, RuntimeError> {
    let session = inner.session.lock().await;
    let preparation = session.build_compaction_preparation_with_window_budget(
        policy,
        resolved_budget,
        window_budget,
        CompactionCoverageBudget::unbounded(),
    )?;
    Ok(preparation.map(|preparation| {
        (
            preparation,
            CompactionRequestBudget {
                policy,
                resolved_budget,
                window_budget,
                primary_window_tokens,
            },
        )
    }))
}

pub(super) async fn compaction_input_for_policy(
    inner: &RuntimeInner,
    policy: CitationCompactionPolicy,
) -> Result<Option<CitationCompactionInput>, RuntimeError> {
    let primary_window = resolved_primary_context_window(inner).await?;
    build_compaction_input(inner, policy, primary_window).await
}

async fn build_compaction_input(
    inner: &RuntimeInner,
    policy: CitationCompactionPolicy,
    primary_window: ResolvedContextWindow,
) -> Result<Option<CitationCompactionInput>, RuntimeError> {
    let resolved_budget = policy.resolve(primary_window.tokens())?;
    let session = inner.session.lock().await;
    session.build_citation_compaction_input(policy, resolved_budget)
}

async fn resolved_primary_context_window(
    inner: &RuntimeInner,
) -> Result<ResolvedContextWindow, RuntimeError> {
    let provider_config = inner.model_config(RuntimeModelRole::Primary).await.ok_or(
        RuntimeError::MissingModelProvider {
            role: RuntimeModelRole::Primary.as_str(),
        },
    )?;
    let context_window_override = inner
        .context_window_tokens
        .read()
        .await
        .map(std::num::NonZeroU64::get);
    resolve_request_context_window(
        provider_config.provider().capabilities(),
        context_window_override,
    )
    .map_err(RuntimeError::from)
}

/// Compiles the stable prefix that a compaction request shares with the agent loop.
///
/// Compaction can run without an active step, so it rebuilds the prefix from the
/// same runtime and session material the step compiler uses. Both paths go
/// through [`compile_stable_prefix_items`], which keeps the provider-visible
/// bytes identical so the provider can reuse the session's cached prefix.
async fn compaction_stable_prefix(
    inner: &RuntimeInner,
) -> Result<Vec<ModelInputItem>, RuntimeError> {
    let (skill_catalog, project_rules) = {
        let session = inner.session.lock().await;
        (session.skill_catalog(), session.project_rules())
    };
    compile_stable_prefix_items(StablePrefixParts {
        prompt_profile: &inner.prompt_profile,
        progress_commentary: inner.progress_commentary,
        skill_catalog: skill_catalog.as_ref(),
        project_rules: project_rules.as_ref(),
    })
    .map_err(|error| RuntimeError::CompactionModelRequest {
        message: error.to_string(),
    })
}

/// Parameters the runtime keeps so it can rebuild a compaction request under a budget.
pub(super) struct CompactionRequestBudget {
    pub(super) policy: CitationCompactionPolicy,
    pub(super) resolved_budget: ResolvedCitationCompactionBudget,
    pub(super) window_budget: CompactionWindowBudget,
    pub(super) primary_window_tokens: u64,
}

/// A compaction request that already fits the compaction model window.
pub(super) struct CompactionPlan {
    input: Box<CitationCompactionInput>,
    request: Box<merry_llm::ModelRequest>,
    /// Reasoning allowance this request was sized with.
    reserve: CompactionReasoningReserve,
}

/// Why one prepared compaction will not replace the checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ArchiveOnlyReason {
    /// The planner itself found no covered window to replace.
    PlanChoseArchiveOnly,
    /// No covered window fit the compaction request budget.
    BudgetExhausted {
        /// Measured input of the smallest request the runtime could build.
        estimated_input_tokens: u64,
        /// Output the window could not afford on top of that input.
        max_output_tokens: u64,
        /// Compaction model window that was too small.
        compactor_window_tokens: u64,
    },
}

impl ArchiveOnlyReason {
    /// Returns the budget failure this degradation ran into, when there was one.
    pub(super) fn budget_failure(self) -> Option<RuntimeError> {
        match self {
            Self::PlanChoseArchiveOnly => None,
            Self::BudgetExhausted {
                estimated_input_tokens,
                max_output_tokens,
                compactor_window_tokens,
            } => Some(RuntimeError::CompactionModelRequestTooLarge {
                estimated_input_tokens,
                max_output_tokens,
                compactor_window_tokens,
            }),
        }
    }
}

/// What the runtime should do for one prepared compaction.
pub(super) enum CompactionAttempt {
    /// The fitted request fits the compaction model window and its reserve.
    Generate(CompactionPlan),
    /// No checkpoint replacement fits; archive tool results without a model call.
    ArchiveOnly {
        input: ArchiveOnlyCompactionInput,
        reason: ArchiveOnlyReason,
    },
}

/// Fits one prepared compaction into the compaction model window.
pub(super) async fn plan_compaction_attempt(
    inner: &Arc<RuntimeInner>,
    preparation: CompactionPreparation,
    budget: &CompactionRequestBudget,
    token: &CancellationToken,
) -> Result<CompactionAttempt, RuntimeError> {
    fit_compaction_plan(
        inner,
        preparation,
        budget,
        CompactionReasoningReserve::INITIAL,
        ReservePolicy::BestEffort,
        token,
    )
    .await
}

/// Returns the reasoning-effort level compaction requests use.
///
/// Compaction never inherits the primary model's effort; the runtime compaction
/// config owns this so automatic and manual compaction agree.
async fn compaction_reasoning_effort(inner: &RuntimeInner) -> Option<ReasoningEffort> {
    inner
        .automatic_compaction
        .read()
        .await
        .reasoning_effort()
        .cloned()
}

/// Fits one prepared compaction under a specific reasoning reserve.
///
/// A request is only returned when the compaction model window can host its input
/// and the output budget `policy` requires, so the provider is never asked for
/// output it cannot deliver. Otherwise the covered window shrinks and the planner
/// re-runs; when no covered window fits, the planner degrades to archiving tool
/// results, which the caller installs or reports.
async fn fit_compaction_plan(
    inner: &Arc<RuntimeInner>,
    preparation: CompactionPreparation,
    budget: &CompactionRequestBudget,
    reserve: CompactionReasoningReserve,
    policy: ReservePolicy,
    token: &CancellationToken,
) -> Result<CompactionAttempt, RuntimeError> {
    if token.is_cancelled() {
        return Err(compaction_cancelled_before_request());
    }
    let provider_config = inner
        .model_config_with_primary_fallback(RuntimeModelRole::ContextCompaction)
        .await
        .ok_or(RuntimeError::MissingModelProvider {
            role: RuntimeModelRole::ContextCompaction.as_str(),
        })?;
    let provider = provider_config.provider();
    let compactor_window_tokens = compaction_model_window(
        provider.capabilities(),
        budget.primary_window_tokens,
        &inner.session_id,
        provider.name(),
    )?;
    let stable_prefix = compaction_stable_prefix(inner).await?;
    let reasoning_effort = compaction_reasoning_effort(inner).await;

    let mut preparation = preparation;
    let mut attempt = 0;
    let mut tightened_coverage = false;
    let mut previous_input_tokens: Option<u64> = None;
    let mut smallest_rejected_request: Option<(u64, u64)> = None;
    loop {
        attempt += 1;
        let input = match preparation {
            CompactionPreparation::ArchiveToolResults(input) => {
                let reason = if tightened_coverage {
                    let Some((estimated_input_tokens, max_output_tokens)) =
                        smallest_rejected_request
                    else {
                        return Err(RuntimeError::Compaction {
                            source: CompactionError::InvalidModelResponseShape {
                                reason: "compaction refit lost its rejection record",
                            },
                        });
                    };
                    ArchiveOnlyReason::BudgetExhausted {
                        estimated_input_tokens,
                        max_output_tokens,
                        compactor_window_tokens,
                    }
                } else {
                    ArchiveOnlyReason::PlanChoseArchiveOnly
                };
                tracing::debug!(
                    event = "runtime.compaction.archive_only_requested",
                    session_id = inner.session_id.as_str(),
                    attempt,
                    ?reason,
                    "compaction keeps every turn raw and archives tool results instead"
                );
                return Ok(CompactionAttempt::ArchiveOnly { input, reason });
            }
            CompactionPreparation::ReplaceCheckpoint(input) => *input,
        };
        let (request, estimated_input_tokens) = match compile_fitted_compaction_request(
            &input,
            provider_config.model(),
            &stable_prefix,
            reasoning_effort.as_ref(),
            compactor_window_tokens,
            reserve,
            policy,
        )? {
            CompactionRequestFit::Request {
                request,
                estimated_input_tokens,
            } => (request, estimated_input_tokens),
            CompactionRequestFit::WindowTooSmall {
                estimated_input_tokens,
                max_output_tokens,
            } => {
                let too_large = RuntimeError::CompactionModelRequestTooLarge {
                    estimated_input_tokens,
                    max_output_tokens,
                    compactor_window_tokens,
                };
                smallest_rejected_request = Some((estimated_input_tokens, max_output_tokens));
                // Re-planning cannot shrink the request any further, so report the
                // budget failure instead of repeating the same plan.
                if previous_input_tokens == Some(estimated_input_tokens) {
                    return Err(too_large);
                }
                let covered_payload_tokens = input
                    .covered_payload_token_estimate()
                    .map_err(|source| RuntimeError::Compaction { source })?;
                // The reserve is a share of the request input, so giving up one
                // token of covered history frees its own reserve as well. Solve
                // for the input the window can host instead of subtracting the
                // raw overshoot, which would give up far more history than needed.
                let allowed_input_tokens = allowed_input_tokens_for_window(
                    compactor_window_tokens,
                    input.resolved_budget().output_token_limit(),
                    reserve,
                );
                let Some(tightened) = tightened_covered_budget(
                    covered_payload_tokens,
                    estimated_input_tokens,
                    allowed_input_tokens,
                ) else {
                    return Err(too_large);
                };
                if attempt >= MAX_COMPACTION_FIT_ATTEMPTS {
                    return Err(too_large);
                }
                tracing::debug!(
                    event = "runtime.compaction.request_refit",
                    session_id = inner.session_id.as_str(),
                    attempt,
                    compactor_window_tokens,
                    estimated_input_tokens,
                    max_output_tokens,
                    covered_payload_tokens,
                    tightened_covered_payload_tokens = tightened,
                    "compaction window cannot host the checkpoint text budget and reasoning reserve; retaining more raw history"
                );
                let coverage = CompactionCoverageBudget::limited(tightened);
                tightened_coverage = true;
                previous_input_tokens = Some(estimated_input_tokens);
                let rebuilt = {
                    let session = inner.session.lock().await;
                    session.build_compaction_preparation_with_window_budget(
                        budget.policy,
                        budget.resolved_budget,
                        budget.window_budget,
                        coverage,
                    )?
                };
                let Some(rebuilt) = rebuilt else {
                    return Err(too_large);
                };
                preparation = rebuilt;
                continue;
            }
        };
        trace_compaction_request(inner, provider.as_ref(), &request, budget, attempt);
        let (_, max_output_tokens) = compaction_request_required_tokens(&request);
        debug_assert!(
            estimated_input_tokens + max_output_tokens <= compactor_window_tokens,
            "a fitted compaction request must fit the compaction model window"
        );
        // Re-check through the shared invariant so the fitting arithmetic and the
        // validation the rest of the runtime relies on cannot drift.
        validate_compaction_model_window(&request, compactor_window_tokens)?;
        return Ok(CompactionAttempt::Generate(CompactionPlan {
            input: Box::new(input),
            request,
            reserve,
        }));
    }
}

/// Generates one compaction candidate and installs it.
///
/// A truncated candidate is never retried with the identical request. A
/// truncation means the reasoning reserve was too small, so the runtime retries
/// once with a larger reserve and a covered window that can host it, then fails
/// explicitly so the caller reports the provider's own truncation reason.
pub(super) async fn generate_and_install_compaction(
    inner: &Arc<RuntimeInner>,
    plan: CompactionPlan,
    budget: &CompactionRequestBudget,
    token: CancellationToken,
    active_permit: &ActiveStepPermit,
) -> Result<CompactionOutcome, RuntimeError> {
    let provider_config = inner
        .model_config_with_primary_fallback(RuntimeModelRole::ContextCompaction)
        .await
        .ok_or(RuntimeError::MissingModelProvider {
            role: RuntimeModelRole::ContextCompaction.as_str(),
        })?;
    let provider = provider_config.provider();
    let mut plan = plan;
    let mut attempt = 0;
    loop {
        attempt += 1;
        if token.is_cancelled() {
            return Err(compaction_cancelled_before_request());
        }
        let stream_context =
            ModelStreamContext::new(token.clone()).with_prompt_cache_key(inner.session_id.clone());
        match generate_validated_compaction_candidate(
            provider.clone(),
            plan.request.as_ref().clone(),
            stream_context,
            &plan.input,
            &token,
        )
        .await
        {
            Ok(candidate_json) => {
                return install_citation_compaction_candidate_transactionally(
                    Arc::clone(inner),
                    *plan.input,
                    &candidate_json,
                    token,
                    active_permit.clone(),
                )
                .await;
            }
            Err(RuntimeError::CompactionModelTruncated { message }) => {
                if attempt > MAX_COMPACTION_TRUNCATION_REFITS {
                    return Err(RuntimeError::CompactionModelTruncated { message });
                }
                let next_reserve = plan.reserve.degraded();
                if next_reserve == plan.reserve {
                    return Err(RuntimeError::CompactionModelTruncated { message });
                }
                // The reserve grew, so the covered window has to shrink for the
                // window to host it. Re-planning from the untightened budget lets
                // the fit loop find that covered window.
                let rebuilt = {
                    let session = inner.session.lock().await;
                    session.build_compaction_preparation_with_window_budget(
                        budget.policy,
                        budget.resolved_budget,
                        budget.window_budget,
                        CompactionCoverageBudget::unbounded(),
                    )?
                };
                let Some(rebuilt) = rebuilt else {
                    return Err(RuntimeError::CompactionModelTruncated { message });
                };
                let CompactionAttempt::Generate(next_plan) = fit_compaction_plan(
                    inner,
                    rebuilt,
                    budget,
                    next_reserve,
                    ReservePolicy::Required,
                    &token,
                )
                .await?
                else {
                    // Archiving tool results cannot fix a truncated checkpoint, and
                    // installing it here would silently change the reduction the
                    // caller announced.
                    return Err(RuntimeError::CompactionModelTruncated { message });
                };
                tracing::debug!(
                    event = "runtime.compaction.truncation_refit",
                    session_id = inner.session_id.as_str(),
                    attempt,
                    reserve_percent = next_reserve.percent(),
                    message,
                    "compaction output was truncated; retrying with a larger reasoning reserve and a smaller covered window"
                );
                plan = next_plan;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Returns the largest request input the window can host for this reserve.
///
/// A request occupies `input + text_budget + input * reserve_percent / 100`, so
/// the input budget is whatever is left after the checkpoint text budget once the
/// reserve share is accounted for.
fn allowed_input_tokens_for_window(
    compactor_window_tokens: u64,
    text_budget_tokens: u64,
    reserve: CompactionReasoningReserve,
) -> u64 {
    compactor_window_tokens
        .saturating_sub(text_budget_tokens)
        .saturating_mul(100)
        / (100 + reserve.percent())
}

/// Returns the covered-payload budget to try after one overshoot.
///
/// Gives up the input the window cannot host plus a margin. Returns `None` when
/// the covered payload is already zero, because retaining more turns cannot
/// shrink the request any further.
fn tightened_covered_budget(
    covered_payload_tokens: u64,
    estimated_input_tokens: u64,
    allowed_input_tokens: u64,
) -> Option<u64> {
    if covered_payload_tokens == 0 {
        return None;
    }
    let excess_input_tokens = estimated_input_tokens.saturating_sub(allowed_input_tokens);
    let margin = excess_input_tokens.saturating_mul(COMPACTION_FIT_MARGIN_PERCENT) / 100;
    let step = excess_input_tokens.saturating_add(margin).max(1);
    let tightened = covered_payload_tokens.saturating_sub(step);
    (tightened < covered_payload_tokens).then_some(tightened)
}

/// How one compiled compaction request fits the compaction model window.
enum CompactionRequestFit {
    /// The request fits and carries the output ceiling it will be sent with.
    Request {
        request: Box<merry_llm::ModelRequest>,
        estimated_input_tokens: u64,
    },
    /// The window cannot host the checkpoint text budget plus the reasoning reserve.
    WindowTooSmall {
        estimated_input_tokens: u64,
        max_output_tokens: u64,
    },
}

/// How strictly one attempt has to afford its reasoning reserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReservePolicy {
    /// Grant the room the window has, as long as the checkpoint text budget fits.
    ///
    /// Used for the first attempt: a small window can still compact by granting
    /// less reasoning room, and refusing outright would stall the session.
    BestEffort,
    /// Only accept a window that can host the checkpoint text budget and the whole reserve.
    ///
    /// Used after the provider truncated an attempt, because best effort is what
    /// produced the truncation. Covering less history is how the retry makes room.
    Required,
}

/// Compiles one compaction request sized for the window and this attempt's reserve.
///
/// The requested output is the checkpoint text budget plus the reasoning reserve.
/// Input size does not depend on that ceiling, so the input is measured first and
/// the ceiling is sized from it. `policy` decides whether the window has to afford
/// the whole reserve or only the text budget; a window that affords neither is
/// reported as `WindowTooSmall` so the caller covers less history.
fn compile_fitted_compaction_request(
    input: &CitationCompactionInput,
    model: &merry_llm::ModelName,
    stable_prefix: &[ModelInputItem],
    reasoning_effort: Option<&ReasoningEffort>,
    compactor_window_tokens: u64,
    reserve: CompactionReasoningReserve,
    policy: ReservePolicy,
) -> Result<CompactionRequestFit, RuntimeError> {
    let compile = |output_ceiling_tokens: u64| {
        compile_citation_compaction_model_request(
            input,
            model,
            stable_prefix,
            reasoning_effort,
            output_ceiling_tokens,
        )
        .map_err(|error| RuntimeError::CompactionModelRequest {
            message: error.to_string(),
        })
    };
    let text_budget_tokens = input.resolved_budget().output_token_limit();
    let measured = compile(text_budget_tokens)?;
    let estimated_input_tokens = compaction_request_required_tokens(&measured).0;
    let reserved_output_tokens =
        reserve.output_ceiling(input.resolved_budget(), estimated_input_tokens);
    let available_output_tokens = compactor_window_tokens.saturating_sub(estimated_input_tokens);
    let affordable_output_tokens = available_output_tokens
        .saturating_sub(compaction_window_safety_tokens(available_output_tokens));
    let output_ceiling_tokens = match policy {
        ReservePolicy::BestEffort => affordable_output_tokens.min(reserved_output_tokens),
        ReservePolicy::Required => reserved_output_tokens,
    };
    let affordable_budget = match policy {
        ReservePolicy::BestEffort => text_budget_tokens,
        ReservePolicy::Required => output_ceiling_tokens,
    };
    if affordable_output_tokens < affordable_budget {
        return Ok(CompactionRequestFit::WindowTooSmall {
            estimated_input_tokens,
            max_output_tokens: reserved_output_tokens,
        });
    }
    let request = if output_ceiling_tokens == text_budget_tokens {
        measured
    } else {
        compile(output_ceiling_tokens)?
    };
    Ok(CompactionRequestFit::Request {
        request: Box::new(request),
        estimated_input_tokens,
    })
}

fn compaction_cancelled_before_request() -> RuntimeError {
    RuntimeError::Compaction {
        source: CompactionError::InvalidModelResponseShape {
            reason: "compaction cancelled before model request",
        },
    }
}

/// Traces one fitted compaction request and its window arithmetic.
fn trace_compaction_request(
    inner: &RuntimeInner,
    provider: &dyn merry_llm::ModelProvider,
    request: &merry_llm::ModelRequest,
    budget: &CompactionRequestBudget,
    attempt: usize,
) {
    let response_format_name = match request.response_format() {
        Some(merry_llm::ModelResponseFormat::StructuredOutput(format)) => format.name(),
        None => "none",
    };
    tracing::debug!(
        event = "runtime.compaction.request",
        session_id = inner.session_id.as_str(),
        provider_name = provider.name().as_str(),
        model = request.model().as_str(),
        attempt,
        message_count = request.messages().len(),
        stable_prefix_message_count = request.stable_prefix_message_count(),
        reasoning_effort = request
            .generation()
            .reasoning_effort()
            .map(merry_llm::ReasoningEffort::as_str),
        estimated_input_tokens = crate::token_estimate::estimate_model_input_tokens(request.input()),
        max_output_tokens = request.generation().max_output_tokens(),
        response_format = response_format_name,
        primary_window_tokens = budget.primary_window_tokens,
        compactor_window_tokens = ?provider.capabilities().max_input_tokens(),
        "compaction model request prepared"
    );
}

const MAX_COMPACTION_FIT_ATTEMPTS: usize = 3;
/// Provider calls one truncated compaction may spend before failing: at most one
/// degraded re-plan on top of the original attempt.
const MAX_COMPACTION_TRUNCATION_REFITS: usize = 1;
/// Extra room one refit gives up beyond the measured overshoot.
const COMPACTION_FIT_MARGIN_PERCENT: u64 = 25;

pub(super) async fn compact_context_once_inner(
    inner: &Arc<RuntimeInner>,
    policy: CitationCompactionPolicy,
    token: CancellationToken,
    active_permit: ActiveStepPermit,
) -> Result<Option<CompactionOutcome>, RuntimeError> {
    if token.is_cancelled() {
        return Err(compaction_cancelled_before_request());
    }

    let primary_window = resolved_primary_context_window(inner).await?;
    let resolved_budget = policy.resolve(primary_window.tokens())?;
    let window_budget = CompactionWindowBudget::unbounded_for_manual_compaction(
        resolved_budget.output_token_limit(),
    )?;
    let budget = CompactionRequestBudget {
        policy,
        resolved_budget,
        window_budget,
        primary_window_tokens: primary_window.tokens(),
    };
    let preparation = {
        let session = inner.session.lock().await;
        session.build_compaction_preparation_with_window_budget(
            policy,
            resolved_budget,
            window_budget,
            CompactionCoverageBudget::unbounded(),
        )?
    };
    let Some(preparation) = preparation else {
        return Ok(None);
    };

    match plan_compaction_attempt(inner, preparation, &budget, &token).await? {
        // Manual compaction keeps its existing contract for the planner's own
        // archive-only choice, but reports an unaffordable request as a failure:
        // the caller asked to compact and the compaction window cannot host any
        // checkpoint replacement.
        CompactionAttempt::ArchiveOnly { reason, .. } => match reason.budget_failure() {
            Some(error) => Err(error),
            None => Ok(None),
        },
        CompactionAttempt::Generate(plan) => {
            generate_and_install_compaction(inner, plan, &budget, token, &active_permit)
                .await
                .map(Some)
        }
    }
}

pub(super) async fn install_citation_compaction_candidate_transactionally(
    inner: Arc<RuntimeInner>,
    input: CitationCompactionInput,
    candidate_json: &str,
    token: CancellationToken,
    active_permit: ActiveStepPermit,
) -> Result<CompactionOutcome, RuntimeError> {
    let outcome = install_compaction_transaction(inner, &token, active_permit, move |session| {
        session.prepare_citation_compaction_install(input, candidate_json)
    })
    .await?;
    Ok(outcome.expect("prepared checkpoint replacement must carry an outcome"))
}

pub(super) async fn install_archive_only_compaction_transactionally(
    inner: Arc<RuntimeInner>,
    input: ArchiveOnlyCompactionInput,
    token: CancellationToken,
    active_permit: ActiveStepPermit,
) -> Result<(), RuntimeError> {
    let outcome = install_compaction_transaction(inner, &token, active_permit, move |session| {
        session.prepare_archive_only_compaction_install(input)
    })
    .await?;
    debug_assert!(
        outcome.is_none(),
        "prepared archive-only install must not carry an outcome"
    );
    Ok(())
}

async fn install_compaction_transaction(
    inner: Arc<RuntimeInner>,
    token: &CancellationToken,
    active_permit: ActiveStepPermit,
    prepare: impl FnOnce(&SessionState) -> Result<PreparedCompactionInstall, RuntimeError>,
) -> Result<Option<CompactionOutcome>, RuntimeError> {
    let store = inner.session_store.clone();
    let mut session = tokio::select! {
        biased;
        () = token.cancelled() => return Err(compaction_cancelled_before_install()),
        session = inner.session.lock() => session,
    };
    if token.is_cancelled() {
        return Err(compaction_cancelled_before_install());
    }

    let prepared = prepare(&session)?;
    let trajectory_snapshot = inner.trajectory.snapshot();
    let bundle = session.persistable_bundle_with_compaction(&prepared, &trajectory_snapshot)?;
    let Some(store) = store else {
        if token.is_cancelled() {
            return Err(compaction_cancelled_before_install());
        }
        session.revalidate_prepared_compaction_install(&prepared)?;
        if token.is_cancelled() {
            return Err(compaction_cancelled_before_install());
        }
        session.set_trajectory_snapshot(trajectory_snapshot);
        return Ok(session.commit_prepared_compaction_install(prepared));
    };
    drop(session);

    let token = token.clone();
    let trace_token = token.clone();
    let session_id = inner.session_id.clone();
    let commit_task = tokio::spawn(async move {
        let result = async {
            if token.is_cancelled() {
                return Err(compaction_cancelled_before_install());
            }
            let staged = store.stage_bundle(bundle).await?;
            complete_staged_compaction(
                inner,
                staged,
                prepared,
                trajectory_snapshot,
                token,
                active_permit,
            )
            .await
        }
        .await;
        if let Err(error) = &result {
            if matches!(error, RuntimeError::SessionStore { .. }) || !trace_token.is_cancelled() {
                tracing::warn!(
                    session_id = %session_id,
                    error = %error,
                    "compaction transaction task failed"
                );
            } else {
                tracing::debug!(
                    session_id = %session_id,
                    error = %error,
                    "compaction transaction task cancelled"
                );
            }
        }
        result
    });
    commit_task
        .await
        .map_err(|error| RuntimeError::CompactionModelStream {
            message: format!("compaction commit task failed: {error}"),
        })?
}

async fn complete_staged_compaction(
    inner: Arc<RuntimeInner>,
    staged: StagedSessionBundle,
    prepared: PreparedCompactionInstall,
    trajectory_snapshot: merry_core::TrajectorySnapshot,
    token: CancellationToken,
    _active_permit: ActiveStepPermit,
) -> Result<Option<CompactionOutcome>, RuntimeError> {
    if token.is_cancelled() {
        return Err(discard_staged_with_error(staged, compaction_cancelled_before_install()).await);
    }

    if let Err(error) = revalidate_staged_compaction(&inner, &token, &prepared).await {
        return Err(discard_staged_with_error(staged, error).await);
    }

    if token.is_cancelled() {
        return Err(discard_staged_with_error(staged, compaction_cancelled_before_install()).await);
    }
    let commit = staged.commit().await?;
    let mut session = inner.session.lock().await;
    session.set_trajectory_snapshot(trajectory_snapshot);
    let outcome = session.commit_prepared_compaction_install(prepared);
    drop(session);
    commit.require_durable()?;
    Ok(outcome)
}

async fn revalidate_staged_compaction(
    inner: &RuntimeInner,
    token: &CancellationToken,
    prepared: &PreparedCompactionInstall,
) -> Result<(), RuntimeError> {
    let session = tokio::select! {
        biased;
        () = token.cancelled() => return Err(compaction_cancelled_before_install()),
        session = inner.session.lock() => session,
    };
    if token.is_cancelled() {
        return Err(compaction_cancelled_before_install());
    }
    session.revalidate_prepared_compaction_install(prepared)
}

async fn discard_staged_with_error(
    staged: StagedSessionBundle,
    error: RuntimeError,
) -> RuntimeError {
    match staged.discard().await {
        Ok(()) => error,
        Err(discard_error) => discard_error.into(),
    }
}

fn compaction_cancelled_before_install() -> RuntimeError {
    RuntimeError::CompactionModelStream {
        message: "compaction cancelled before checkpoint install".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Numbers from the session that exposed the collapsing retry.
    ///
    /// The compaction window was 272,000 tokens, the checkpoint text budget
    /// 21,760, the covered payload 359,176, and the fitted first attempt measured
    /// 397,849 input tokens. At a doubled reserve the old arithmetic gave up
    /// 433,166 tokens of history and collapsed coverage to zero, which degraded a
    /// recoverable truncation into a failed step.
    #[test]
    fn proportional_reserve_refit_keeps_a_usable_covered_window() {
        let window = 272_000;
        let text_budget = 21_760;
        let covered_payload = 359_176;
        let measured_input = 397_849;
        let reserve = CompactionReasoningReserve::INITIAL.degraded();

        assert_eq!(reserve.percent(), 50);
        let allowed_input = allowed_input_tokens_for_window(window, text_budget, reserve);
        let tightened = tightened_covered_budget(covered_payload, measured_input, allowed_input)
            .expect("a proportional refit must keep some covered window");

        assert!(
            tightened > 0,
            "the refit must not collapse coverage to zero"
        );
        let projected_input = measured_input - (covered_payload - tightened);
        let projected_output = reserve.output_ceiling(
            CitationCompactionPolicy::default()
                .resolve(window)
                .expect("budget resolves"),
            projected_input,
        );
        assert!(
            projected_input + projected_output <= window,
            "refitted request must fit the window: input {projected_input} plus output {projected_output}"
        );
    }

    #[test]
    fn reserve_shrinks_the_input_budget_monotonically() {
        let window = 272_000;
        let text_budget = 21_760;

        let initial = allowed_input_tokens_for_window(
            window,
            text_budget,
            CompactionReasoningReserve::INITIAL,
        );
        let degraded = allowed_input_tokens_for_window(
            window,
            text_budget,
            CompactionReasoningReserve::INITIAL.degraded(),
        );
        assert!(
            degraded < initial,
            "a larger reserve must leave room for less input: {initial} then {degraded}"
        );
    }

    /// A window that cannot host the text budget admits no covered history.
    #[test]
    fn window_smaller_than_the_text_budget_admits_no_input() {
        assert_eq!(
            allowed_input_tokens_for_window(16_000, 21_760, CompactionReasoningReserve::INITIAL),
            0
        );
    }
}
