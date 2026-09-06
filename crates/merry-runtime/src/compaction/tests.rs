mod prompt_payload;
mod schema;

use super::{
    CitationCompactionModelTurn, CitationCompactionTurnItem, CompactionWindowFingerprint,
    CompactionWindowPlan,
};
use merry_core::{ArtifactId, EvidenceLocator, EvidenceRef};
use std::collections::BTreeSet;

fn test_window(
    history_id: u64,
    ref_id: &str,
    text: &str,
) -> (Vec<CitationCompactionModelTurn>, CompactionWindowPlan) {
    let turn_id = crate::session::ModelTurnId::new(1);
    let window = vec![
        CitationCompactionModelTurn::new(
            turn_id,
            crate::session::ModelTurnStatus::Completed,
            vec![CitationCompactionTurnItem::user(
                history_id,
                ref_id.to_owned(),
                text.to_owned(),
            )],
        )
        .expect("valid test turn"),
    ];
    let plan = CompactionWindowPlan::new(
        vec![turn_id],
        Vec::new(),
        BTreeSet::new(),
        Some(turn_id),
        CompactionWindowFingerprint::new(0),
    );
    (window, plan)
}

fn evidence(artifact_id: &str) -> EvidenceRef {
    EvidenceRef::new(
        ArtifactId::new(artifact_id).expect("valid artifact id"),
        EvidenceLocator::whole_artifact(),
    )
}
