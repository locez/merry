//! Public Rust facade for Merry.
//!
//! This crate is the interface layer for applications embedding Merry. It
//! exposes small components for runtime construction: providers, profiles,
//! tools, events, and stable errors. Lower-level crates still own their
//! domains.

pub mod agent_loop;
pub mod errors;
pub mod events;
pub mod profiles;
pub mod providers;
pub mod tools;

pub use agent_loop::{coding_agent_loop_config, generic_agent_loop_config};
pub use merry_core::SessionId;
pub use merry_llm::ModelName;
pub use merry_runtime::{
    AgentLoopConfig, AgentLoopResult, AgentLoopStatus, Runtime, RuntimeBuilder, RuntimeError,
    RuntimeProfile, RuntimeProfileBuilder,
};
