//! Permission admission tests for the workspace runner, grouped by policy.
//!
//! `path_rules` covers trusted rules, review materialization, session
//! retention, and the deny/read-only ceilings. `host_integrations` covers
//! trusted-config preauthorization. `network` covers the network ceiling.
//! `factory_capabilities` covers how the host factory answers requests.

mod factory_capabilities;
mod host_integrations;
mod network;
mod path_rules;
