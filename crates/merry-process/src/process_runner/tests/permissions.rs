//! Permission admission tests for the sandbox runner factories, grouped by the
//! policy each group owns.
//!
//! `path_rules` covers trusted rules, review materialization, session
//! retention, and the deny/read-only ceilings; `git_baseline` the automatic Git
//! metadata protection; `host_integrations` the trusted-config
//! preauthorization; `network` the network ceiling; and `factory_capabilities`
//! how the non-sandbox factories answer requested capabilities.

mod factory_capabilities;
mod git_baseline;
mod host_integrations;
mod network;
mod path_rules;
