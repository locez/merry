use super::{
    ActivatedMemory, MemoryActivationReason, MemoryActivationScore, MemoryActivationSeed,
    MemoryError, MemoryId, MemoryItem,
};
use std::collections::{BTreeMap, BTreeSet};

/// Pure deterministic internal memory activator.
#[derive(Debug, Default)]
pub(crate) struct MemoryActivator;

impl MemoryActivator {
    pub(crate) fn activate(
        seed: &MemoryActivationSeed,
        candidates: &[MemoryItem],
    ) -> Result<Vec<ActivatedMemory>, MemoryError> {
        let query = seed.query();
        let mut eligible = Vec::new();
        let mut seen_ids = BTreeSet::new();

        for item in candidates {
            if !seen_ids.insert(item.id().clone()) {
                return Err(MemoryError::DuplicateMemoryId {
                    id: item.id().clone(),
                });
            }

            if !seed.allows_scope(item.scope()) {
                continue;
            }

            let matched_triggers = matched_triggers(query, item.triggers());
            if matched_triggers.is_empty() {
                continue;
            }

            let score = MemoryActivationScore {
                trigger_matches: matched_triggers.len(),
                priority: item.priority(),
                confidence: item.confidence(),
            };

            let mut reasons = Vec::with_capacity(matched_triggers.len() + 2);
            reasons.push(MemoryActivationReason::ScopeAllowed);
            for trigger in matched_triggers {
                reasons.push(MemoryActivationReason::trigger_matched(trigger)?);
            }
            reasons.push(MemoryActivationReason::ranked(score));

            eligible.push(ActivatedMemory::new(
                item.clone(),
                score,
                reasons,
                seed.provenance().clone(),
            )?);
        }

        eligible.sort_by(|left, right| {
            right
                .score()
                .cmp(&left.score())
                .then_with(|| left.item().id().cmp(right.item().id()))
        });

        resolve_conflicts(eligible)
    }
}

fn matched_triggers(query_lowercase: &str, triggers: &[String]) -> Vec<String> {
    triggers
        .iter()
        .filter(|trigger| query_lowercase.contains(trigger.as_str()))
        .cloned()
        .collect()
}

fn resolve_conflicts(
    activations: Vec<ActivatedMemory>,
) -> Result<Vec<ActivatedMemory>, MemoryError> {
    let mut selected = Vec::with_capacity(activations.len());
    let mut conflict_winners: BTreeMap<String, usize> = BTreeMap::new();
    let mut suppressed_by_winner: BTreeMap<usize, Vec<MemoryId>> = BTreeMap::new();

    for activation in activations {
        if let Some(conflict_key) = activation.item().conflict_key() {
            let conflict_key = conflict_key.to_owned();
            if let Some(winner_index) = conflict_winners.get(&conflict_key) {
                suppressed_by_winner
                    .entry(*winner_index)
                    .or_default()
                    .push(activation.item().id().clone());
                continue;
            }

            conflict_winners.insert(conflict_key, selected.len());
        }

        selected.push(activation);
    }

    for (winner_index, suppressed) in suppressed_by_winner {
        selected[winner_index].add_reason(MemoryActivationReason::conflict_winner(suppressed)?)?;
    }

    Ok(selected)
}
