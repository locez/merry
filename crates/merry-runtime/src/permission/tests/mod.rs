//! Permission admission tests, grouped by the module each group owns.
//!
//! `input` covers request parsing and the `request_permissions` input schema,
//! `review` the model reviewer contract, `channel` the host review transport,
//! and `request_json` the shared request/JSON layout. `call` is the
//! pending-call fixture every group parses.

use super::*;
use merry_core::{ToolCallArguments, ToolCallId};
use serde_json::Value;

mod channel;
mod input;
mod request_json;
mod review;

/// Builds the `request_permissions` pending call each group parses.
pub(super) fn call(arguments: Value) -> PendingToolCall {
    PendingToolCall::new(
        ToolCallId::new("call-permission").expect("valid id"),
        ToolName::new("request_permissions").expect("valid tool name"),
        ToolCallArguments::try_from(arguments).expect("valid arguments"),
    )
}
