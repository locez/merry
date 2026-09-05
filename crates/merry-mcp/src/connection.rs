use crate::{
    McpError, McpServerConfig,
    catalog::{filter_allowed_tools, mcp_tool_to_catalog_entry},
    client::McpHttpClient,
    diagnostics::{McpDiscoveryStage, McpFailureKind},
    map_mcp_tool_names,
};
use merry_core::{SessionToolCatalogEntry, ToolBindingName, ToolSourceFingerprint, ToolSourceId};
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
    entries: Vec<SessionToolCatalogEntry>,
}

#[derive(Debug)]
enum ConnectionState {
    Disconnected {
        failure: Option<DiscoveryFailure>,
        retry_at: Instant,
    },
    Ready(ReadyConnection),
}

#[derive(Debug)]
pub(crate) struct McpConnection {
    config: McpServerConfig,
    source: ToolSourceId,
    fingerprint: ToolSourceFingerprint,
    state: Mutex<ConnectionState>,
}

#[derive(Debug)]
pub(crate) enum ToolUnavailable {
    Discovery(DiscoveryFailure),
    DefinitionChanged,
}

impl McpConnection {
    pub(crate) fn new(config: McpServerConfig) -> Result<Self, McpError> {
        let (source, fingerprint) = config.validated_identity()?;
        Ok(Self {
            config,
            source,
            fingerprint,
            state: Mutex::new(ConnectionState::Disconnected {
                failure: None,
                retry_at: Instant::now(),
            }),
        })
    }

    pub(crate) fn source(&self) -> &ToolSourceId {
        &self.source
    }

    pub(crate) fn fingerprint(&self) -> &ToolSourceFingerprint {
        &self.fingerprint
    }

    pub(crate) fn allows(&self, name: &ToolBindingName) -> bool {
        self.config
            .tools()
            .is_none_or(|allowed| allowed.iter().any(|raw| raw == name.as_str()))
    }

    pub(crate) async fn discover(
        &self,
        deadline: Instant,
    ) -> Result<Vec<SessionToolCatalogEntry>, DiscoveryFailure> {
        let mut state = self.state.lock().await;
        if Instant::now() >= deadline {
            let failure = DiscoveryFailure {
                stage: McpDiscoveryStage::Initialize,
                kind: McpFailureKind::Timeout,
            };
            *state = ConnectionState::Disconnected {
                failure: Some(failure),
                retry_at: Instant::now() + RECONNECT_COOLDOWN,
            };
            return Err(failure);
        }
        self.refresh(&mut state, deadline).await?;
        state
            .as_ready()
            .map(|ready| ready.entries.clone())
            .ok_or(DiscoveryFailure {
                stage: McpDiscoveryStage::ListTools,
                kind: McpFailureKind::Transport,
            })
    }

    pub(crate) async fn client_for(
        &self,
        expected: &SessionToolCatalogEntry,
    ) -> Result<Arc<McpHttpClient>, ToolUnavailable> {
        let mut state = self.state.lock().await;
        if let ConnectionState::Disconnected { failure, retry_at } = &*state {
            if let Some(failure) = *failure
                && (!failure.kind.retryable() || Instant::now() < *retry_at)
            {
                return Err(ToolUnavailable::Discovery(failure));
            }
            self.refresh(&mut state, Instant::now() + DISCOVERY_TIMEOUT)
                .await
                .map_err(ToolUnavailable::Discovery)?;
        }
        let ready = state.as_ready().ok_or(ToolUnavailable::DefinitionChanged)?;
        if !ready
            .entries
            .iter()
            .any(|live| definition_matches(live, expected))
        {
            return Err(ToolUnavailable::DefinitionChanged);
        }
        Ok(Arc::clone(&ready.client))
    }

    pub(crate) async fn record_call_failure(&self, client: &Arc<McpHttpClient>, error: &McpError) {
        let mut state = self.state.lock().await;
        if state
            .as_ready()
            .is_some_and(|ready| Arc::ptr_eq(&ready.client, client))
        {
            *state = ConnectionState::Disconnected {
                failure: Some(DiscoveryFailure {
                    stage: McpDiscoveryStage::Initialize,
                    kind: McpFailureKind::from_error(error),
                }),
                retry_at: Instant::now() + RECONNECT_COOLDOWN,
            };
        }
    }

    async fn refresh(
        &self,
        state: &mut ConnectionState,
        deadline: Instant,
    ) -> Result<(), DiscoveryFailure> {
        let mut stage = McpDiscoveryStage::Initialize;
        let result =
            tokio::time::timeout_at(deadline.min(Instant::now() + DISCOVERY_TIMEOUT), async {
                let client = Arc::new(McpHttpClient::new(self.config.clone())?);
                client.initialize().await?;
                stage = McpDiscoveryStage::ListTools;
                let entries = self.list_entries(&client).await?;
                Ok::<_, McpError>(ReadyConnection { client, entries })
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
        *state = ConnectionState::Disconnected {
            failure: Some(failure),
            retry_at: Instant::now() + RECONNECT_COOLDOWN,
        };
        Err(failure)
    }

    async fn list_entries(
        &self,
        client: &McpHttpClient,
    ) -> Result<Vec<SessionToolCatalogEntry>, McpError> {
        let mut tools = Vec::new();
        let mut cursor = None;
        let mut seen_cursors = BTreeSet::new();
        loop {
            let page = client.list_tools(cursor.as_deref()).await?;
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
        tools.sort_by(|left, right| left.name.cmp(&right.name));
        let names = tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        if names.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(McpError::InvalidJson {
                server_id: self.source.to_string(),
                message: "duplicate tool names".to_owned(),
            });
        }
        let mappings = map_mcp_tool_names(self.source.as_str(), &names)?;
        tools
            .into_iter()
            .zip(mappings)
            .map(|(tool, mapping)| {
                mcp_tool_to_catalog_entry(
                    self.source.as_str(),
                    &self.fingerprint,
                    &tool,
                    mapping.merry_name,
                )
            })
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

pub(crate) fn definition_matches(
    live: &SessionToolCatalogEntry,
    saved: &SessionToolCatalogEntry,
) -> bool {
    live.binding() == saved.binding()
        && live.spec().description() == saved.spec().description()
        && live.spec().input_schema() == saved.spec().input_schema()
}
