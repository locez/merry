//! Async Chrome DevTools Protocol (CDP) JSON-RPC client and telemetry ring buffers.

use super::ws::{WsMessage, WsReader, WsWriter};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

const MAX_CONSOLE_ENTRIES: usize = 200;
const MAX_NETWORK_ENTRIES: usize = 200;

/// Recorded browser console message or uncaught exception.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConsoleEntry {
    pub(crate) level: String,
    pub(crate) text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) source_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) line: Option<u64>,
}

/// Recorded browser network request lifecycle entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NetworkEntry {
    pub(crate) request_id: String,
    pub(crate) method: String,
    pub(crate) url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) resource_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) status_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) response_headers: Option<Value>,
    pub(crate) finished: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) encoded_data_length: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error_text: Option<String>,
}

#[derive(Default)]
struct TelemetryState {
    console: VecDeque<ConsoleEntry>,
    network_order: VecDeque<String>,
    network_entries: HashMap<String, NetworkEntry>,
}

impl TelemetryState {
    fn push_console(&mut self, entry: ConsoleEntry) {
        if self.console.len() >= MAX_CONSOLE_ENTRIES {
            self.console.pop_front();
        }
        self.console.push_back(entry);
    }

    fn upsert_network_request(
        &mut self,
        request_id: String,
        method: String,
        url: String,
        resource_type: Option<String>,
    ) {
        if !self.network_entries.contains_key(&request_id) {
            if self.network_order.len() >= MAX_NETWORK_ENTRIES
                && let Some(evicted) = self.network_order.pop_front()
            {
                self.network_entries.remove(&evicted);
            }
            self.network_order.push_back(request_id.clone());
        }
        self.network_entries.insert(
            request_id.clone(),
            NetworkEntry {
                request_id,
                method,
                url,
                resource_type,
                status: None,
                status_text: None,
                mime_type: None,
                response_headers: None,
                finished: false,
                encoded_data_length: None,
                error_text: None,
            },
        );
    }

    fn update_network_response(
        &mut self,
        request_id: &str,
        status: u16,
        status_text: Option<String>,
        mime_type: Option<String>,
        headers: Option<Value>,
        resource_type: Option<String>,
    ) {
        if let Some(entry) = self.network_entries.get_mut(request_id) {
            entry.status = Some(status);
            entry.status_text = status_text;
            entry.mime_type = mime_type;
            entry.response_headers = headers;
            if entry.resource_type.is_none() {
                entry.resource_type = resource_type;
            }
        }
    }

    fn finish_network_request(&mut self, request_id: &str, encoded_data_length: Option<u64>) {
        if let Some(entry) = self.network_entries.get_mut(request_id) {
            entry.finished = true;
            entry.encoded_data_length = encoded_data_length;
        }
    }

    fn fail_network_request(&mut self, request_id: &str, error_text: Option<String>) {
        if let Some(entry) = self.network_entries.get_mut(request_id) {
            entry.finished = true;
            entry.error_text = error_text.or_else(|| Some("request failed".to_owned()));
        }
    }
}

enum OutgoingCommand {
    Text(String),
    Pong(Vec<u8>),
    Close,
}

/// Active multiplexed CDP session.
pub(crate) struct CdpSession {
    next_id: AtomicU64,
    closed: Arc<AtomicBool>,
    outgoing_tx: mpsc::Sender<OutgoingCommand>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>,
    telemetry: Arc<Mutex<TelemetryState>>,
    tasks: Vec<JoinHandle<()>>,
}

impl CdpSession {
    /// Starts the CDP multiplex tasks over an established WebSocket connection.
    pub(crate) fn from_ws<R, W>(mut ws_reader: WsReader<R>, mut ws_writer: WsWriter<W>) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<OutgoingCommand>(64);
        let pending = Arc::new(Mutex::new(HashMap::<
            u64,
            oneshot::Sender<Result<Value, String>>,
        >::new()));
        let telemetry = Arc::new(Mutex::new(TelemetryState::default()));
        let closed = Arc::new(AtomicBool::new(false));

        let writer_closed = Arc::clone(&closed);
        let writer_handle = tokio::spawn(async move {
            while let Some(cmd) = outgoing_rx.recv().await {
                match cmd {
                    OutgoingCommand::Text(payload) => {
                        if ws_writer.send_text(&payload).await.is_err() {
                            break;
                        }
                    }
                    OutgoingCommand::Pong(payload) => {
                        if ws_writer.send_pong(&payload).await.is_err() {
                            break;
                        }
                    }
                    OutgoingCommand::Close => {
                        let _ = ws_writer.send_close().await;
                        break;
                    }
                }
            }
            writer_closed.store(true, Ordering::SeqCst);
        });

        let reader_pending = Arc::clone(&pending);
        let reader_telemetry = Arc::clone(&telemetry);
        let reader_closed = Arc::clone(&closed);
        let reader_outgoing = outgoing_tx.clone();
        let reader_handle = tokio::spawn(async move {
            loop {
                match ws_reader.next_message().await {
                    Ok(WsMessage::Text(text)) => {
                        handle_cdp_message(&text, &reader_pending, &reader_telemetry);
                    }
                    Ok(WsMessage::Ping(payload)) => {
                        let _ = reader_outgoing.try_send(OutgoingCommand::Pong(payload));
                    }
                    Ok(WsMessage::Close) | Err(_) => break,
                }
            }
            reader_closed.store(true, Ordering::SeqCst);
            let _ = reader_outgoing.try_send(OutgoingCommand::Close);
            let mut pending_map = reader_pending
                .lock()
                .expect("CDP pending map lock poisoned");
            for (_, tx) in pending_map.drain() {
                let _ = tx.send(Err("CDP WebSocket connection closed".to_owned()));
            }
        });

        Self {
            next_id: AtomicU64::new(1),
            closed,
            outgoing_tx,
            pending,
            telemetry,
            tasks: vec![writer_handle, reader_handle],
        }
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Sends one CDP command and awaits its JSON-RPC result or error.
    pub(crate) async fn call(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        if self.is_closed() {
            return Err("CDP session is closed".to_owned());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let payload = serde_json::to_string(&json!({
            "id": id,
            "method": method,
            "params": params,
        }))
        .map_err(|e| format!("failed to encode CDP request {method}: {e}"))?;

        let (tx, rx) = oneshot::channel();
        {
            let mut map = self.pending.lock().expect("CDP pending lock poisoned");
            map.insert(id, tx);
        }

        if self
            .outgoing_tx
            .send(OutgoingCommand::Text(payload))
            .await
            .is_err()
        {
            let mut map = self.pending.lock().expect("CDP pending lock poisoned");
            map.remove(&id);
            return Err("CDP WebSocket writer closed before sending request".to_owned());
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(format!("CDP response channel dropped for {method}")),
            Err(_) => {
                let mut map = self.pending.lock().expect("CDP pending lock poisoned");
                map.remove(&id);
                Err(format!("CDP call {method} timed out after {timeout:?}"))
            }
        }
    }

    /// Returns buffered console entries, optionally draining them.
    pub(crate) fn console_entries(&self, clear: bool) -> Vec<ConsoleEntry> {
        let mut telemetry = self.telemetry.lock().expect("telemetry lock poisoned");
        let entries: Vec<_> = telemetry.console.iter().cloned().collect();
        if clear {
            telemetry.console.clear();
        }
        entries
    }

    /// Returns captured network requests in chronological order.
    pub(crate) fn network_entries(&self, clear: bool) -> Vec<NetworkEntry> {
        let mut telemetry = self.telemetry.lock().expect("telemetry lock poisoned");
        let entries = telemetry
            .network_order
            .iter()
            .filter_map(|id| telemetry.network_entries.get(id).cloned())
            .collect();
        if clear {
            telemetry.network_order.clear();
            telemetry.network_entries.clear();
        }
        entries
    }

    /// Looks up a single captured network entry by its lenient request ID.
    pub(crate) fn network_entry(&self, request_id: &str) -> Option<NetworkEntry> {
        let telemetry = self.telemetry.lock().expect("telemetry lock poisoned");
        telemetry.network_entries.get(request_id).cloned()
    }

    /// Closes the CDP WebSocket and shuts down background tasks.
    pub(crate) async fn close(mut self) {
        self.closed.store(true, Ordering::SeqCst);
        let _ = self.outgoing_tx.send(OutgoingCommand::Close).await;
        for handle in self.tasks.drain(..) {
            handle.abort();
            let _ = handle.await;
        }
    }
}

impl Drop for CdpSession {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        let _ = self.outgoing_tx.try_send(OutgoingCommand::Close);
        for handle in &self.tasks {
            handle.abort();
        }
    }
}

#[derive(Deserialize)]
struct CdpInbound {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<CdpRpcError>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Deserialize)]
struct CdpRpcError {
    code: i64,
    message: String,
}

fn handle_cdp_message(
    raw: &str,
    pending: &Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>,
    telemetry: &Arc<Mutex<TelemetryState>>,
) {
    let Ok(msg) = serde_json::from_str::<CdpInbound>(raw) else {
        return;
    };

    if let Some(id) = msg.id {
        let tx = {
            let mut map = pending.lock().expect("CDP pending lock poisoned");
            map.remove(&id)
        };
        if let Some(tx) = tx {
            let outcome = if let Some(err) = msg.error {
                Err(format!("CDP error {}: {}", err.code, err.message))
            } else {
                Ok(msg.result.unwrap_or(Value::Null))
            };
            let _ = tx.send(outcome);
        }
        return;
    }

    if let (Some(method), Some(params)) = (msg.method, msg.params) {
        record_cdp_event(&method, params, telemetry);
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConsoleApiCalledParams {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    args: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExceptionThrownParams {
    exception_details: ExceptionDetails,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExceptionDetails {
    text: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    line_number: Option<u64>,
    #[serde(default)]
    exception: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestWillBeSentParams {
    #[serde(deserialize_with = "deserialize_lenient_string")]
    request_id: String,
    request: CdpRequestPayload,
    #[serde(rename = "type", default)]
    resource_type: Option<String>,
}

#[derive(Deserialize)]
struct CdpRequestPayload {
    method: String,
    url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResponseReceivedParams {
    #[serde(deserialize_with = "deserialize_lenient_string")]
    request_id: String,
    response: CdpResponsePayload,
    #[serde(rename = "type", default)]
    resource_type: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CdpResponsePayload {
    status: u16,
    #[serde(default)]
    status_text: Option<String>,
    #[serde(default)]
    mime_type: Option<String>,
    #[serde(default)]
    headers: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoadingFinishedParams {
    #[serde(deserialize_with = "deserialize_lenient_string")]
    request_id: String,
    #[serde(default)]
    encoded_data_length: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoadingFailedParams {
    #[serde(deserialize_with = "deserialize_lenient_string")]
    request_id: String,
    #[serde(default)]
    error_text: Option<String>,
}

fn record_cdp_event(method: &str, params: Value, telemetry: &Arc<Mutex<TelemetryState>>) {
    match method {
        "Runtime.consoleAPICalled" => {
            if let Ok(parsed) = serde_json::from_value::<ConsoleApiCalledParams>(params) {
                let text = parsed
                    .args
                    .iter()
                    .map(|arg| {
                        arg.get("value")
                            .or_else(|| arg.get("description"))
                            .map(|v| match v {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            })
                            .unwrap_or_else(|| arg.to_string())
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let mut state = telemetry.lock().expect("telemetry lock poisoned");
                state.push_console(ConsoleEntry {
                    level: parsed.kind.unwrap_or_else(|| "log".to_owned()),
                    text,
                    source_url: None,
                    line: None,
                });
            }
        }
        "Runtime.exceptionThrown" => {
            if let Ok(parsed) = serde_json::from_value::<ExceptionThrownParams>(params) {
                let detail = parsed.exception_details;
                let extra = detail
                    .exception
                    .as_ref()
                    .and_then(|v| v.get("description"))
                    .and_then(Value::as_str);
                let text = match extra {
                    Some(desc) => format!("{}: {desc}", detail.text),
                    None => detail.text,
                };
                let mut state = telemetry.lock().expect("telemetry lock poisoned");
                state.push_console(ConsoleEntry {
                    level: "exception".to_owned(),
                    text,
                    source_url: detail.url,
                    line: detail.line_number,
                });
            }
        }
        "Network.requestWillBeSent" => {
            if let Ok(parsed) = serde_json::from_value::<RequestWillBeSentParams>(params) {
                let mut state = telemetry.lock().expect("telemetry lock poisoned");
                state.upsert_network_request(
                    parsed.request_id,
                    parsed.request.method,
                    parsed.request.url,
                    parsed.resource_type,
                );
            }
        }
        "Network.responseReceived" => {
            if let Ok(parsed) = serde_json::from_value::<ResponseReceivedParams>(params) {
                let mut state = telemetry.lock().expect("telemetry lock poisoned");
                state.update_network_response(
                    &parsed.request_id,
                    parsed.response.status,
                    parsed.response.status_text,
                    parsed.response.mime_type,
                    parsed.response.headers,
                    parsed.resource_type,
                );
            }
        }
        "Network.loadingFinished" => {
            if let Ok(parsed) = serde_json::from_value::<LoadingFinishedParams>(params) {
                let bytes = parsed
                    .encoded_data_length
                    .filter(|n| *n >= 0.0)
                    .map(|n| n as u64);
                let mut state = telemetry.lock().expect("telemetry lock poisoned");
                state.finish_network_request(&parsed.request_id, bytes);
            }
        }
        "Network.loadingFailed" => {
            if let Ok(parsed) = serde_json::from_value::<LoadingFailedParams>(params) {
                let mut state = telemetry.lock().expect("telemetry lock poisoned");
                state.fail_network_request(&parsed.request_id, parsed.error_text);
            }
        }
        _ => {}
    }
}

/// Deserializes CDP IDs whether emitted as JSON strings (`"62576.34"`) or numbers (`62576.34`).
pub(crate) fn deserialize_lenient_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::String(s) => Ok(s),
        Value::Number(n) => Ok(n.to_string()),
        other => Err(serde::de::Error::custom(format!(
            "expected string or number for CDP identifier, got {other}"
        ))),
    }
}

/// Optional variant of [`deserialize_lenient_string`].
pub(crate) fn deserialize_lenient_opt_string<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(Value::Number(n)) => Ok(Some(n.to_string())),
        Some(other) => Err(serde::de::Error::custom(format!(
            "expected string or number for CDP identifier, got {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lenient_string_deserializes_strings_and_numbers() {
        #[derive(Deserialize)]
        struct Sample {
            #[serde(deserialize_with = "deserialize_lenient_string")]
            request_id: String,
        }

        let str_id: Sample =
            serde_json::from_str(r#"{"request_id":"62576.34"}"#).expect("string id parses");
        assert_eq!(str_id.request_id, "62576.34");

        let num_id: Sample =
            serde_json::from_str(r#"{"request_id":62576.34}"#).expect("float id parses");
        assert_eq!(num_id.request_id, "62576.34");

        let int_id: Sample = serde_json::from_str(r#"{"request_id":42}"#).expect("int id parses");
        assert_eq!(int_id.request_id, "42");
    }
}
