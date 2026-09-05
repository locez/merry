use crate::{
    McpError, McpResult, McpServerConfig,
    adapter::{McpExecutionBinding, McpToolRegistration},
    catalog::mcp_catalog,
    connection::{DiscoveryFailure, McpConnection, STARTUP_TIMEOUT, definition_matches},
    diagnostics::{McpDiscoveryStage, McpFailureKind, McpServerDiagnostic, McpServerIssue},
};
use futures_util::{StreamExt, stream};
use merry_core::{SessionToolCatalog, ToolSourceId};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tokio::time::Instant;

/// Partial discovery results. Network and remote-protocol failures are diagnostics,
/// not an instruction for the application to exit.
#[derive(Debug)]
pub struct McpDiscovery {
    registrations: Vec<McpToolRegistration>,
    diagnostics: Vec<McpServerDiagnostic>,
}

impl McpDiscovery {
    fn new(registrations: Vec<McpToolRegistration>, diagnostics: Vec<McpServerDiagnostic>) -> Self {
        Self {
            registrations,
            diagnostics,
        }
    }

    /// Returns registered tools in deterministic or restored session order.
    #[must_use]
    pub fn registrations(&self) -> &[McpToolRegistration] {
        &self.registrations
    }

    /// Returns safe diagnostics which the caller must present to the user.
    #[must_use]
    pub fn diagnostics(&self) -> &[McpServerDiagnostic] {
        &self.diagnostics
    }

    /// Transfers executors and diagnostics to the application surface.
    #[must_use]
    pub fn into_parts(self) -> (Vec<McpToolRegistration>, Vec<McpServerDiagnostic>) {
        (self.registrations, self.diagnostics)
    }
}

/// Discovers a new catalog with bounded concurrency and a ten-second startup budget.
///
/// Local configuration errors are fatal before network IO. A remote failure only
/// omits that server's tools and emits a diagnostic. Dropping this future cancels
/// discovery; no background task is spawned.
pub async fn discover_mcp_tools(servers: &[McpServerConfig]) -> McpResult<McpDiscovery> {
    prepare(servers, None).await
}

/// Rebinds the exact saved catalog, retaining offline and revoked definitions.
///
/// Only sources still authorized by the current configuration may connect.
/// New sources and tools are not added. Matching cached definitions may reconnect
/// on later execution attempts after a cooldown, without replaying any tool call.
pub async fn restore_mcp_tools(
    servers: &[McpServerConfig],
    catalog: &SessionToolCatalog,
) -> McpResult<McpDiscovery> {
    prepare(servers, Some(catalog)).await
}

async fn prepare(
    servers: &[McpServerConfig],
    catalog: Option<&SessionToolCatalog>,
) -> McpResult<McpDiscovery> {
    let mut connections = BTreeMap::new();
    for config in servers {
        let connection = Arc::new(McpConnection::new(config.clone())?);
        if connections
            .insert(connection.source().clone(), connection)
            .is_some()
        {
            return Err(McpError::InvalidConfiguration {
                reason: "duplicate MCP server ids",
            });
        }
    }
    let mcp_catalog = catalog.map(mcp_catalog).transpose()?;
    let catalog = mcp_catalog.as_ref();

    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let selected = connections
        .values()
        .filter(|connection| {
            catalog.is_none_or(|catalog| {
                catalog.entries().iter().any(|entry| {
                    entry.binding().source() == connection.source()
                        && entry.binding().source_fingerprint() == connection.fingerprint()
                        && connection.allows(entry.binding().operation())
                })
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut results = stream::iter(selected)
        .map(|connection| async move {
            (
                connection.source().clone(),
                connection.discover(deadline).await,
            )
        })
        .buffer_unordered(4)
        .collect::<BTreeMap<_, _>>()
        .await;

    if catalog.is_none() {
        let mut owners = BTreeMap::new();
        let mut conflicts = BTreeSet::new();
        for (source, result) in &results {
            if let Ok(entries) = result {
                for entry in entries {
                    if let Some(owner) = owners.insert(entry.spec().name().clone(), source.clone())
                    {
                        conflicts.insert(owner);
                        conflicts.insert(source.clone());
                    }
                }
            }
        }
        for source in conflicts {
            results.insert(
                source,
                Err(DiscoveryFailure {
                    stage: McpDiscoveryStage::ListTools,
                    kind: McpFailureKind::InvalidToolDefinition,
                }),
            );
        }
    }

    let mut registrations = Vec::new();
    let mut diagnostics = Vec::new();
    for (source, result) in &results {
        if let Err(failure) = result {
            push_diagnostic(
                &mut diagnostics,
                source,
                McpServerIssue::Unavailable {
                    stage: failure.stage,
                    failure: failure.kind,
                },
                catalog,
            );
        }
    }
    if let Some(catalog) = catalog {
        for entry in catalog.entries() {
            let source = entry.binding().source();
            let binding = match connections.get(source) {
                None => disabled(
                    &mut diagnostics,
                    source,
                    McpServerIssue::NotConfigured,
                    catalog,
                ),
                Some(connection)
                    if entry.binding().source_fingerprint() != connection.fingerprint() =>
                {
                    disabled(
                        &mut diagnostics,
                        source,
                        McpServerIssue::EndpointChanged,
                        catalog,
                    )
                }
                Some(connection) if !connection.allows(entry.binding().operation()) => disabled(
                    &mut diagnostics,
                    source,
                    McpServerIssue::ToolsDisallowed,
                    catalog,
                ),
                Some(connection) => {
                    if let Some(Ok(live)) = results.get(source)
                        && !live.iter().any(|live| definition_matches(live, entry))
                    {
                        push_diagnostic(
                            &mut diagnostics,
                            source,
                            McpServerIssue::CatalogChanged,
                            Some(catalog),
                        );
                    }
                    McpExecutionBinding::Connection(Arc::clone(connection))
                }
            };
            registrations.push(McpToolRegistration::new(entry.clone(), binding));
        }
        for source in connections.keys() {
            if !catalog
                .entries()
                .iter()
                .any(|entry| entry.binding().source() == source)
            {
                push_diagnostic(
                    &mut diagnostics,
                    source,
                    McpServerIssue::NotInSessionCatalog,
                    Some(catalog),
                );
            }
        }
        for (source, live) in &results {
            if let Ok(live) = live
                && live.iter().any(|entry| {
                    !catalog
                        .entries()
                        .iter()
                        .any(|saved| saved.binding() == entry.binding())
                })
            {
                push_diagnostic(
                    &mut diagnostics,
                    source,
                    McpServerIssue::CatalogChanged,
                    Some(catalog),
                );
            }
        }
    } else {
        let mut entries = Vec::new();
        for (source, result) in results {
            if let Ok(discovered) = result {
                let connection =
                    connections
                        .get(&source)
                        .ok_or(McpError::InvalidConfiguration {
                            reason: "discovered MCP source has no configured binding",
                        })?;
                for entry in discovered {
                    entries.push(entry.clone());
                    registrations.push(McpToolRegistration::new(
                        entry,
                        McpExecutionBinding::Connection(Arc::clone(connection)),
                    ));
                }
            }
        }
        SessionToolCatalog::new(entries)?;
    }
    diagnostics.sort_by(|left, right| left.server_id().cmp(right.server_id()));
    Ok(McpDiscovery::new(registrations, diagnostics))
}

fn disabled(
    diagnostics: &mut Vec<McpServerDiagnostic>,
    source: &ToolSourceId,
    issue: McpServerIssue,
    catalog: &SessionToolCatalog,
) -> McpExecutionBinding {
    push_diagnostic(diagnostics, source, issue.clone(), Some(catalog));
    McpExecutionBinding::Disabled(issue)
}

fn push_diagnostic(
    diagnostics: &mut Vec<McpServerDiagnostic>,
    source: &ToolSourceId,
    issue: McpServerIssue,
    catalog: Option<&SessionToolCatalog>,
) {
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.server_id() == source && diagnostic.issue() == &issue)
    {
        return;
    }
    let count = catalog.map_or(0, |catalog| {
        catalog
            .entries()
            .iter()
            .filter(|entry| entry.binding().source() == source)
            .count()
    });
    diagnostics.push(McpServerDiagnostic::new(source.clone(), issue, count));
}
