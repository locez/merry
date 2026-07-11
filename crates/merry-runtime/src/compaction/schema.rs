use crate::checkpoint::compacted_checkpoint_candidate_schema;
use schemars::Schema;

pub fn citation_compaction_response_schema() -> Schema {
    compacted_checkpoint_candidate_schema()
}
