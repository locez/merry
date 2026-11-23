use crate::{
    McpError, McpServerConfig,
    catalog::{DiscoveredMcpTool, filter_allowed_tools, parse_mcp_tool},
    client::McpHttpClient,
    diagnostics::{McpDiscoveryStage, McpFailureKind},
};
use merry_core::ToolSourceId;
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use tokio::{sync::Mutex, time::Instant};

pub(crate) const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const RECONNECT_COOLDOWN: Duration = Duration::from_secs(5);
const MAX_TOOLS: usize = 1024;
const MAX_PAGES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DiscoveryFailure {
    pub(crate) stage: McpDiscoveryStage,
    pub(crate) kind: McpFailureKind,
}

#[derive(Debug)]
struct ReadyConnection {
    client: Arc<McpHttpClient>,
    tools: Vec<DiscoveredMcpTool>,
}

#[derive(Debug)]
enum ConnectionState {
    Disconnected {
        failure: Option<DiscoveryFailure>,
        retry_at: Instant,
    },
    Ready(ReadyConnection),
}

/// Live tool snapshot for one server returned to `mcp_search`.
#[derive(Debug)]
pub(crate) enum ServerToolsState {
    Ready { tools: Vec<DiscoveredMcpTool> },
    Unavailable { failure: DiscoveryFailure },
}

/// Failure modes when resolving a tool for `mcp_call`.
#[derive(Debug)]
pub(crate) enum CallLookupError {
    Unavailable(DiscoveryFailure),
    ToolDisallowed,
    ToolNotFound { available_tools: Vec<String> },
}

/// Resolved live client and tool definition for `mcp_call`.
pub(crate) struct ResolvedToolCall {
    pub(crate) client: Arc<McpHttpClient>,
    pub(crate) tool: DiscoveredMcpTool,
    pub(crate) refreshed: bool,
}

#[derive(Debug)]
pub(crate) struct McpConnection {
    config: McpServerConfig,
    source: ToolSourceId,
    state: Mutex<ConnectionState>,
}

impl McpConnection {
    pub(crate) fn new(config: McpServerConfig) -> Result<Self, McpError> {
        let source = config.validated_source()?;
        Ok(Self {
            config,
            source,
            state: Mutex::new(ConnectionState::Disconnected {
                failure: None,
                retry_at: Instant::now(),
            }),
        })
    }

    pub(crate) fn source(&self) -> &ToolSourceId {
        &self.source
    }

    pub(crate) fn allows_name(&self, name: &str) -> bool {
        self.config
            .tools()
            .is_none_or(|allowed| allowed.iter().any(|raw| raw == name))
    }

    /// Runs the initial bounded startup discovery against this server.
    pub(crate) async fn discover(
        &self,
        deadline: Instant,
    ) -> Result<Vec<DiscoveredMcpTool>, DiscoveryFailure> {
        let mut state = self.state.lock().await;
        if Instant::now() >= deadline {
            return Err(mark_disconnected(
                &mut state,
                DiscoveryFailure {
                    stage: McpDiscoveryStage::Initialize,
                    kind: McpFailureKind::Timeout,
                },
            ));
        }
        self.reconnect_and_list(&mut state, deadline).await?;
        state
            .as_ready()
            .map(|ready| ready.tools.clone())
            .ok_or(DiscoveryFailure {
                stage: McpDiscoveryStage::ListTools,
                kind: McpFailureKind::Transport,
            })
    }

    /// Returns the server's current tool snapshot, automatically refreshing
    /// when `notifications/tools/list_changed` arrived, when `force_refresh` is
    /// set, or when a retryable disconnect cooldown has elapsed.
    pub(crate) async fn tools_snapshot(&self, force_refresh: bool) -> ServerToolsState {
        let mut state = self.state.lock().await;
        match &mut *state {
            ConnectionState::Ready(ready) => {
                if force_refresh || ready.client.take_tools_dirty() {
                    let deadline = Instant::now() + DISCOVERY_TIMEOUT;
                    if let Err(failure) =
                        self.refresh_ready_or_reconnect(&mut state, deadline).await
                    {
                        return ServerToolsState::Unavailable { failure };
                    }
                }
            }
            ConnectionState::Disconnected { failure, retry_at } => {
                if !force_refresh
                    && let Some(failure) = *failure
                    && (!failure.kind.retryable() || Instant::now() < *retry_at)
                {
                    return ServerToolsState::Unavailable { failure };
                }
                let deadline = Instant::now() + DISCOVERY_TIMEOUT;
                if let Err(failure) = self.reconnect_and_list(&mut state, deadline).await {
                    return ServerToolsState::Unavailable { failure };
                }
            }
        }
        match &*state {
            ConnectionState::Ready(ready) => ServerToolsState::Ready {
                tools: ready.tools.clone(),
            },
            ConnectionState::Disconnected { failure, .. } => ServerToolsState::Unavailable {
                failure: failure.unwrap_or(DiscoveryFailure {
                    stage: McpDiscoveryStage::Initialize,
                    kind: McpFailureKind::Transport,
                }),
            },
        }
    }

    /// Resolves an active client and live tool definition for `mcp_call`.
    ///
    /// Automatically refreshes if `force_refresh` is set, if the server sent
    /// `notifications/tools/list_changed`, or if the requested tool is absent
    /// from the cached snapshot (allowing newly deployed remote tools or
    /// updated schemas to work without restarting the session).
    pub(crate) async fn resolve_for_call(
        &self,
        tool_name: &str,
        force_refresh: bool,
    ) -> Result<ResolvedToolCall, CallLookupError> {
        if !self.allows_name(tool_name) {
            return Err(CallLookupError::ToolDisallowed);
        }
        let mut state = self.state.lock().await;
        let mut refreshed = false;

        match &mut *state {
            ConnectionState::Disconnected { failure, retry_at } => {
                if !force_refresh
                    && let Some(failure) = *failure
                    && (!failure.kind.retryable() || Instant::now() < *retry_at)
                {
                    return Err(CallLookupError::Unavailable(failure));
                }
                self.reconnect_and_list(&mut state, Instant::now() + DISCOVERY_TIMEOUT)
                    .await
                    .map_err(CallLookupError::Unavailable)?;
                refreshed = true;
            }
            ConnectionState::Ready(ready) if force_refresh || ready.client.take_tools_dirty() => {
                self.refresh_ready_or_reconnect(&mut state, Instant::now() + DISCOVERY_TIMEOUT)
                    .await
                    .map_err(CallLookupError::Unavailable)?;
                refreshed = true;
            }
            ConnectionState::Ready(_) => {}
        }

        if let Some((client, tool)) = find_tool_in_state(&state, tool_name) {
            return Ok(ResolvedToolCall {
                client,
                tool,
                refreshed,
            });
        }

        if !refreshed {
            self.refresh_ready_or_reconnect(&mut state, Instant::now() + DISCOVERY_TIMEOUT)
                .await
                .map_err(CallLookupError::Unavailable)?;
            if let Some((client, tool)) = find_tool_in_state(&state, tool_name) {
                return Ok(ResolvedToolCall {
                    client,
                    tool,
                    refreshed: true,
                });
            }
        }

        let available_tools = state
            .as_ready()
            .map(|ready| ready.tools.iter().map(|tool| tool.name.clone()).collect())
            .unwrap_or_default();
        Err(CallLookupError::ToolNotFound { available_tools })
    }

    pub(crate) async fn record_call_failure(&self, client: &Arc<McpHttpClient>, error: &McpError) {
        let mut state = self.state.lock().await;
        if state
            .as_ready()
            .is_some_and(|ready| Arc::ptr_eq(&ready.client, client))
        {
            mark_disconnected(
                &mut state,
                DiscoveryFailure {
                    stage: McpDiscoveryStage::Initialize,
                    kind: McpFailureKind::from_error(error),
                },
            );
        }
    }

    /// Re-fetches `tools/list` on the existing live session; if the existing
    /// session fails (e.g., expired or closed), falls back to a fresh handshake.
    async fn refresh_ready_or_reconnect(
        &self,
        state: &mut ConnectionState,
        deadline: Instant,
    ) -> Result<(), DiscoveryFailure> {
        let existing_client = state.as_ready().map(|ready| Arc::clone(&ready.client));
        if let Some(client) = existing_client {
            let list_result = tokio::time::timeout_at(
                deadline.min(Instant::now() + DISCOVERY_TIMEOUT),
                self.list_tools_on(&client),
            )
            .await;
            match list_result {
                Ok(Ok(tools)) => {
                    *state = ConnectionState::Ready(ReadyConnection { client, tools });
                    return Ok(());
                }
                Ok(Err(error)) => {
                    let failure_kind = McpFailureKind::from_error(&error);
                    if !failure_kind.retryable() {
                        return Err(mark_disconnected(
                            state,
                            DiscoveryFailure {
                                stage: McpDiscoveryStage::ListTools,
                                kind: failure_kind,
                            },
                        ));
                    }
                }
                Err(_) => {
                    return Err(mark_disconnected(
                        state,
                        DiscoveryFailure {
                            stage: McpDiscoveryStage::ListTools,
                            kind: McpFailureKind::Timeout,
                        },
                    ));
                }
            }
        }
        self.reconnect_and_list(state, deadline).await
    }

    async fn reconnect_and_list(
        &self,
        state: &mut ConnectionState,
        deadline: Instant,
    ) -> Result<(), DiscoveryFailure> {
        let mut stage = McpDiscoveryStage::Initialize;
        let result =
            tokio::time::timeout_at(deadline.min(Instant::now() + DISCOVERY_TIMEOUT), async {
                let client = Arc::new(McpHttpClient::connect(self.config.clone()).await?);
                stage = McpDiscoveryStage::ListTools;
                let tools = self.list_tools_on(&client).await?;
                Ok::<_, McpError>(ReadyConnection { client, tools })
            })
            .await;
        let failure = match result {
            Ok(Ok(ready)) => {
                *state = ConnectionState::Ready(ready);
                return Ok(());
            }
            Ok(Err(error)) => DiscoveryFailure {
                stage,
                kind: McpFailureKind::from_error(&error),
            },
            Err(_) => DiscoveryFailure {
                stage,
                kind: McpFailureKind::Timeout,
            },
        };
        Err(mark_disconnected(state, failure))
    }

    async fn list_tools_on(
        &self,
        client: &McpHttpClient,
    ) -> Result<Vec<DiscoveredMcpTool>, McpError> {
        let mut tools = Vec::new();
        let mut cursor = None;
        let mut seen_cursors = BTreeSet::new();
        loop {
            let page = client.list_tools(cursor).await?;
            tools.extend(page.tools);
            if tools.len() > MAX_TOOLS {
                return Err(McpError::ResponseTooLarge {
                    server_id: self.source.to_string(),
                });
            }
            cursor = page.next_cursor;
            let Some(next) = &cursor else {
                break;
            };
            if !seen_cursors.insert(next.clone()) || seen_cursors.len() >= MAX_PAGES {
                return Err(McpError::InvalidJson {
                    server_id: self.source.to_string(),
                    message: "invalid or excessive tool pagination".to_owned(),
                });
            }
        }
        let mut tools = filter_allowed_tools(tools, self.config.tools());
        tools.sort_by(|left, right| left.name.as_ref().cmp(right.name.as_ref()));
        if tools
            .windows(2)
            .any(|pair| pair[0].name.as_ref() == pair[1].name.as_ref())
        {
            return Err(McpError::InvalidJson {
                server_id: self.source.to_string(),
                message: "duplicate tool names".to_owned(),
            });
        }
        tools
            .iter()
            .map(|tool| parse_mcp_tool(self.source.as_str(), tool))
            .collect()
    }
}

impl ConnectionState {
    fn as_ready(&self) -> Option<&ReadyConnection> {
        match self {
            Self::Ready(ready) => Some(ready),
            Self::Disconnected { .. } => None,
        }
    }
}

fn mark_disconnected(state: &mut ConnectionState, failure: DiscoveryFailure) -> DiscoveryFailure {
    *state = ConnectionState::Disconnected {
        failure: Some(failure),
        retry_at: Instant::now() + RECONNECT_COOLDOWN,
    };
    failure
}

fn find_tool_in_state(
    state: &ConnectionState,
    tool_name: &str,
) -> Option<(Arc<McpHttpClient>, DiscoveredMcpTool)> {
    let ready = state.as_ready()?;
    let tool = ready.tools.iter().find(|tool| tool.name == tool_name)?;
    Some((Arc::clone(&ready.client), tool.clone()))
}
