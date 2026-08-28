//! Runtime-owned interactive agent loop.
//!
//! The public module is a narrow facade over the interactive producer and its
//! focused queue, command, plan, settings, and handle components.

mod commands;
mod handles;
mod plan;
mod producer;
mod queue;
mod settings;
mod types;

use crate::{AgentLoopConfig, Runtime, RuntimeError, StepContext};
use producer::{InteractiveProducer, InteractiveProducerInput};
use std::sync::{Arc, atomic::AtomicU64};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

pub use self::handles::{
    AgentLoopControl, AgentLoopInput, InteractiveAgentRun, InteractiveInputItem,
    InteractiveInputSnapshot, InteractiveRunEventStream, InteractiveRunMessage,
};
pub use self::settings::{
    InteractivePrimaryModel, InteractiveSettingsUpdate, InteractiveSubagentSettings,
};
pub use self::types::{InteractiveError, InteractiveRunId, InterruptReason};

impl Runtime {
    pub fn start_interactive_agent_run(
        &self,
        context: StepContext,
        config: AgentLoopConfig,
    ) -> Result<InteractiveAgentRun, RuntimeError> {
        let loop_permit = self.acquire_active_step_permit()?;
        let (parent_token, generation_config, context_contract) = context.into_parts();
        let config = config
            .merge_context_final_output_contract(context_contract)
            .map_err(|source| RuntimeError::AgentLoopConfig { source })?;
        let loop_token = parent_token.child_token();
        let producer_token = loop_token.clone();
        let subagent_completion_notify = self.subagent_completion_notify();
        let run_id = types::next_interactive_run_id();
        let (message_sender, message_receiver) = mpsc::channel(16);
        let (command_sender, command_receiver) = mpsc::channel(16);
        let (bridge_sender, bridge_receiver) = mpsc::channel(4);
        let bridge_resolution_epoch = Arc::new(AtomicU64::new(0));
        let plan_event_receiver = self.subscribe_plan_events();
        let producer = InteractiveProducer::new(InteractiveProducerInput {
            runtime: self.clone(),
            command_receiver,
            plan_event_receiver,
            subagent_completion_notify,
            message_sender,
            bridge_receiver,
            bridge_resolution_epoch: Arc::clone(&bridge_resolution_epoch),
            loop_token: producer_token,
            generation_config,
            config,
            loop_permit,
        });
        let producer_handle = tokio::spawn(async move { producer.run().await });

        Ok(InteractiveAgentRun::new(
            InteractiveRunEventStream::new(
                run_id,
                ReceiverStream::new(message_receiver),
                loop_token,
                producer_handle,
                bridge_sender,
                bridge_resolution_epoch,
            ),
            AgentLoopInput::new(run_id, command_sender.clone()),
            AgentLoopControl::new(run_id, command_sender),
        ))
    }
}
