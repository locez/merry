use super::super::{
    PlanError,
    protocol::{PlanDecompositionInput, ReportPlanAttemptInput},
    validation,
};
use merry_core::PlanAttemptOutcome;

pub(super) fn validate_attempt_report_contract(
    input: &ReportPlanAttemptInput,
) -> Result<(), PlanError> {
    match input.outcome {
        PlanAttemptOutcome::Completed
            if input.result.is_some()
                && input.diagnostic.is_none()
                && input.decomposition.is_none() => {}
        PlanAttemptOutcome::Decomposed
            if input.result.is_none()
                && input.diagnostic.is_none()
                && input.decomposition.is_some() =>
        {
            validate_decomposition(input.decomposition.as_ref().expect("matched some"))?;
        }
        PlanAttemptOutcome::Blocked | PlanAttemptOutcome::SemanticFailure
            if input.decomposition.is_none()
                && (input.result.is_some() || input.diagnostic.is_some()) => {}
        PlanAttemptOutcome::TransientFailure
            if input.result.is_none()
                && input.diagnostic.is_some()
                && input.decomposition.is_none() => {}
        PlanAttemptOutcome::Yielded if input.result.is_none() && input.decomposition.is_none() => {}
        PlanAttemptOutcome::Cancelled | PlanAttemptOutcome::Interrupted => {
            return Err(PlanError::InvalidAttemptOutcome {
                outcome: input.outcome,
            });
        }
        _ => {
            return Err(PlanError::InvalidAttemptOutcome {
                outcome: input.outcome,
            });
        }
    }
    Ok(())
}

fn validate_decomposition(input: &PlanDecompositionInput) -> Result<(), PlanError> {
    validation::validate_reason(&input.reason)?;
    if input.children.is_empty() {
        return Err(PlanError::EmptyDecomposition);
    }
    if input
        .children
        .iter()
        .any(|child| !child.children.is_empty())
    {
        return Err(PlanError::NestedDecomposition);
    }
    Ok(())
}
