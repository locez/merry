use crate::{McpDiscovery, McpServerConfig};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use futures_util::StreamExt;
use merry_core::{
    RuntimeJournalEvent, RuntimeJournalPayload, SessionId, ToolCallId, ToolCallResult, ToolName,
};
use merry_llm::{
    FinishReason, ModelEvent, ModelName, ModelOutput, ModelResponse, ModelToolCall,
    ModelToolCallId, ToolArguments, testing::FakeModelProvider,
};
use merry_runtime::{Runtime, StepContext, StepInput};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub(super) enum Mode {
    Online,
    Status(u16),
    StallInitialize,
    StallCall,
    InvalidJson,
    RpcSecret,
    Oversized,
    Redirect(String),
    Paginated,
}

#[derive(Clone)]
struct Settings {
    mode: Mode,
    tools: Vec<Value>,
}

struct ServerState {
    settings: Mutex<Settings>,
    events: mpsc::Sender<String>,
    calls: AtomicUsize,
    requests: AtomicUsize,
    expire_session: AtomicBool,
    shutdown: CancellationToken,
}

pub(super) struct Server {
    address: SocketAddr,
    state: Arc<ServerState>,
    events: mpsc::Receiver<String>,
    handle: Option<JoinHandle<std::io::Result<()>>>,
}

impl Server {
    pub(super) async fn start(mode: Mode, tools: Vec<Value>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, events) = mpsc::channel(128);
        let state = Arc::new(ServerState {
            settings: Mutex::new(Settings { mode, tools }),
            events: sender,
            calls: AtomicUsize::new(0),
            requests: AtomicUsize::new(0),
            expire_session: AtomicBool::new(false),
            shutdown: CancellationToken::new(),
        });
        let app = Router::new()
            .route("/mcp", post(handle))
            .with_state(Arc::clone(&state));
        let shutdown = state.shutdown.clone();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
        });
        Self {
            address,
            state,
            events,
            handle: Some(handle),
        }
    }

    pub(super) fn config(&self, id: &str) -> McpServerConfig {
        McpServerConfig::builder(id, self.url()).build()
    }
    pub(super) fn url(&self) -> String {
        format!("http://{}/mcp", self.address)
    }
    pub(super) fn calls(&self) -> usize {
        self.state.calls.load(Ordering::SeqCst)
    }
    pub(super) fn requests(&self) -> usize {
        self.state.requests.load(Ordering::SeqCst)
    }
    pub(super) fn set_mode(&self, mode: Mode) {
        self.state.settings.lock().unwrap().mode = mode;
    }
    pub(super) fn set_tools(&self, tools: Vec<Value>) {
        self.state.settings.lock().unwrap().tools = tools;
    }
    pub(super) fn expire_session(&self) {
        self.state.expire_session.store(true, Ordering::SeqCst);
    }
    pub(super) async fn wait_for(&mut self, method: &str) {
        loop {
            let observed =
                tokio::time::timeout(std::time::Duration::from_secs(10), self.events.recv())
                    .await
                    .unwrap()
                    .unwrap();
            if observed == method {
                return;
            }
        }
    }
    pub(super) async fn stop(mut self) {
        self.state.shutdown.cancel();
        self.handle.take().unwrap().await.unwrap().unwrap();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.state.shutdown.cancel();
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

#[derive(Deserialize)]
struct WireRequest {
    id: Option<u64>,
    method: String,
    params: Option<Value>,
}

async fn handle(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(request): Json<WireRequest>,
) -> Response {
    state.requests.fetch_add(1, Ordering::SeqCst);
    let _ = state.events.try_send(request.method.clone());
    if request.method == "tools/call" {
        state.calls.fetch_add(1, Ordering::SeqCst);
    }
    if request.method == "tools/call"
        && state.expire_session.swap(false, Ordering::SeqCst)
        && headers.contains_key("mcp-session-id")
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let settings = state.settings.lock().unwrap().clone();
    match &settings.mode {
        Mode::Status(status) => return StatusCode::from_u16(*status).unwrap().into_response(),
        Mode::StallInitialize if request.method == "initialize" => { state.shutdown.cancelled().await; return StatusCode::SERVICE_UNAVAILABLE.into_response(); }
        Mode::StallCall if request.method == "tools/call" => { state.shutdown.cancelled().await; return StatusCode::SERVICE_UNAVAILABLE.into_response(); }
        Mode::InvalidJson => return ([("content-type", "application/json")], "not-json secret-query-value").into_response(),
        Mode::RpcSecret => return Json(json!({"jsonrpc":"2.0", "id":request.id, "error":{"code":-32603,"message":"secret-query-value", "data":{"credential":"secret-query-value"}}})).into_response(),
        Mode::Oversized => return ([("content-type", "application/json")], "x".repeat(crate::client::MAX_RESPONSE_BYTES + 1)).into_response(),
        Mode::Redirect(target) => return (StatusCode::TEMPORARY_REDIRECT, [("location", target)]).into_response(),
        _ => {}
    }
    let result = match request.method.as_str() {
        "initialize" => {
            json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
        }
        "notifications/initialized" => return StatusCode::ACCEPTED.into_response(),
        "tools/list" if matches!(settings.mode, Mode::Paginated) => {
            if request
                .params
                .as_ref()
                .and_then(|params| params.get("cursor"))
                .is_some()
            {
                json!({"tools": settings.tools.iter().skip(1).collect::<Vec<_>>()})
            } else {
                json!({"tools": settings.tools.iter().take(1).collect::<Vec<_>>(),"nextCursor":"next"})
            }
        }
        "tools/list" => json!({"tools":settings.tools}),
        "tools/call" => json!({"content":[{"type":"text","text":"done"}],"isError":false}),
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    let mut response =
        Json(json!({"jsonrpc":"2.0","id":request.id,"result":result})).into_response();
    if request.method == "initialize" {
        response.headers_mut().insert(
            "Mcp-Session-Id",
            HeaderValue::from_static("fixture-session"),
        );
    }
    response
}

pub(super) fn tool(name: &str) -> Value {
    json!({"name":name,"description":format!("Tool {name}"),"inputSchema":{"type":"object","properties":{}}})
}

pub(super) fn text_provider() -> FakeModelProvider {
    FakeModelProvider::new(vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
    })])
}

pub(super) fn call_provider(name: &ToolName, count: usize) -> FakeModelProvider {
    FakeModelProvider::new_turns(
        (0..count)
            .map(|index| {
                vec![Ok(ModelEvent::Completed {
                    response: ModelResponse::new(
                        vec![ModelOutput::tool_call(ModelToolCall::new(
                            ModelToolCallId::new(&format!("call-{index}")).unwrap(),
                            name.clone(),
                            ToolArguments::new(Default::default()),
                        ))],
                        FinishReason::ToolCalls,
                        None,
                    ),
                })]
            })
            .collect(),
    )
}

pub(super) fn runtime(id: &str, discovery: McpDiscovery, provider: &FakeModelProvider) -> Runtime {
    let builder = Runtime::builder(SessionId::new(id).unwrap())
        .fully_trusted_tools()
        .model_provider(
            Arc::new(provider.clone()),
            ModelName::new("fixture").unwrap(),
        );
    discovery
        .into_parts()
        .0
        .into_iter()
        .fold(builder, |builder, registration| {
            builder.register_tool(registration.tool)
        })
        .build()
        .unwrap()
}

pub(super) async fn step(runtime: &Runtime) {
    let events = runtime
        .step(
            StepInput::user_text("continue").unwrap(),
            StepContext::default(),
        )
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    assert!(!events.is_empty());
}

pub(super) async fn next_call(runtime: &Runtime) -> ToolCallId {
    step(runtime).await;
    runtime
        .pending_tool_calls()
        .await
        .first()
        .unwrap()
        .id()
        .clone()
}

pub(super) fn resolved(events: &[RuntimeJournalEvent]) -> &ToolCallResult {
    events
        .iter()
        .find_map(|event| match &event.payload {
            RuntimeJournalPayload::ToolCallResolved { result } => Some(result),
            _ => None,
        })
        .expect("tool call resolves durably")
}
