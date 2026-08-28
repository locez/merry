//! Interactive producer state and module wiring.

mod commands;
mod lifecycle;
mod model;
mod output;
mod tools;

use super::{
    commands::InteractiveCommand, handles::InteractiveRunMessage, queue::InteractiveInputQueue,
    types::InteractiveError,
};
use crate::{AgentLoopConfig, Runtime, bridge::BridgeToolResultCommand, events::ActiveStepPermit};
use merry_llm::GenerationConfig;
use std::collections::BTreeSet;
use std::sync::{Arc, atomic::AtomicU64};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

pub(super) struct InteractiveProducer {
    pub(super) runtime: Runtime,
    pub(super) queue: InteractiveInputQueue,
    pub(super) command_receiver: mpsc::Receiver<InteractiveCommand>,
    pub(super) plan_event_receiver: crate::plan::PlanControllerEventReceiver,
    pub(super) subagent_completion_notify: Option<Arc<Notify>>,
    pub(super) message_sender: mpsc::Sender<InteractiveRunMessage>,
    pub(super) bridge_receiver: mpsc::Receiver<BridgeToolResultCommand>,
    pub(super) bridge_resolution_epoch: Arc<AtomicU64>,
    pub(super) bridge_pending: bool,
    pub(super) bridge_batch_sequence: u64,
    pub(super) loop_token: CancellationToken,
    pub(super) generation_config: GenerationConfig,
    pub(super) config: AgentLoopConfig,
    pub(super) loop_permit: ActiveStepPermit,
    pub(super) suspended_resume_requested: bool,
    pub(super) phase_token: Option<CancellationToken>,
    pub(super) interrupted: bool,
    pub(super) seen_plan_sequences: BTreeSet<u64>,
    pub(super) coordinator_continuation_requested: bool,
    pub(super) coordinator_continuation_note: Option<String>,
    pub(super) subagent_continuation_requested: bool,
    pub(super) model_turns_run: usize,
    pub(super) structured_output_retries: usize,
    pub(super) terminal_error: Option<InteractiveError>,
}

pub(super) struct InteractiveProducerInput {
    pub(super) runtime: Runtime,
    pub(super) command_receiver: mpsc::Receiver<InteractiveCommand>,
    pub(super) plan_event_receiver: crate::plan::PlanControllerEventReceiver,
    pub(super) subagent_completion_notify: Option<Arc<Notify>>,
    pub(super) message_sender: mpsc::Sender<InteractiveRunMessage>,
    pub(super) bridge_receiver: mpsc::Receiver<BridgeToolResultCommand>,
    pub(super) bridge_resolution_epoch: Arc<AtomicU64>,
    pub(super) loop_token: CancellationToken,
    pub(super) generation_config: GenerationConfig,
    pub(super) config: AgentLoopConfig,
    pub(super) loop_permit: ActiveStepPermit,
}

impl InteractiveProducer {
    pub(super) fn new(input: InteractiveProducerInput) -> Self {
        let InteractiveProducerInput {
            runtime,
            command_receiver,
            plan_event_receiver,
            subagent_completion_notify,
            message_sender,
            bridge_receiver,
            bridge_resolution_epoch,
            loop_token,
            generation_config,
            config,
            loop_permit,
        } = input;
        Self {
            runtime,
            queue: InteractiveInputQueue::default(),
            command_receiver,
            plan_event_receiver,
            subagent_completion_notify,
            message_sender,
            bridge_receiver,
            bridge_resolution_epoch,
            bridge_pending: false,
            bridge_batch_sequence: 0,
            loop_token,
            generation_config,
            config,
            loop_permit,
            suspended_resume_requested: false,
            phase_token: None,
            interrupted: false,
            seen_plan_sequences: std::collections::BTreeSet::new(),
            coordinator_continuation_requested: false,
            coordinator_continuation_note: None,
            subagent_continuation_requested: false,
            model_turns_run: 0,
            structured_output_retries: 0,
            terminal_error: None,
        }
    }
}

pub(super) use output::is_plan_payload;
