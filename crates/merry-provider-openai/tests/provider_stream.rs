use futures_util::TryStreamExt;
use merry_llm::{
    FinishReason, GenerationConfig, ModelContent, ModelError, ModelEvent, ModelMessage,
    ModelMessageRole, ModelName, ModelOutput, ModelProvider, ModelRequest, ModelResponse,
    ModelStreamContext, ProviderErrorKind, Usage,
};
use merry_provider_openai::{OpenAiProvider, OpenAiProviderConfig};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
};
use tokio_util::sync::CancellationToken;

fn request() -> ModelRequest {
    ModelRequest::new(
        ModelName::new("debug-model").expect("valid model name"),
        vec![
            ModelMessage::new(
                ModelMessageRole::User,
                ModelContent::text("Hello").expect("valid content"),
            )
            .expect("valid message"),
        ],
        Vec::new(),
        GenerationConfig::default(),
    )
    .expect("valid request")
}

fn provider(base_url: &str) -> OpenAiProvider {
    let config = OpenAiProviderConfig::new("sk-test")
        .expect("valid config")
        .with_base_url(base_url)
        .expect("valid base url")
        .with_organization("org-test")
        .expect("valid organization")
        .with_project("proj-test")
        .expect("valid project");
    OpenAiProvider::new(config)
}

fn expect_setup_error(result: Result<merry_llm::ModelEventStream, ModelError>) -> ModelError {
    match result {
        Ok(_) => panic!("stream setup should fail"),
        Err(error) => error,
    }
}

#[ignore = "requires loopback TCP permission; default tests cover this behavior without network"]
#[tokio::test]
async fn stream_model_posts_responses_request_and_streams_events() {
    let body = concat!(
        "data: {\"type\":\"response.created\"}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello\"}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\" world\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello world\"}]}],\"usage\":{\"input_tokens\":9,\"output_tokens\":3}}}\n\n",
        "data: [DONE]\n\n",
    );
    let server = TestServer::spawn(TestResponse::ok_sse(body));
    let stream = provider(server.base_url())
        .stream_model(request(), ModelStreamContext::default())
        .await
        .expect("stream setup should succeed");

    let events = stream
        .try_collect::<Vec<_>>()
        .await
        .expect("stream succeeds");

    assert_eq!(
        events,
        vec![
            ModelEvent::Started,
            ModelEvent::OutputTextDelta {
                delta: "Hello".to_owned()
            },
            ModelEvent::OutputTextDelta {
                delta: " world".to_owned()
            },
            ModelEvent::Completed {
                response: ModelResponse::new(
                    vec![ModelOutput::text("Hello world")],
                    FinishReason::Stop,
                    Some(Usage::new(9, 3)),
                )
            },
        ]
    );

    let received = server.received();
    assert_eq!(received.method, "POST");
    assert_eq!(received.path, "/responses");
    assert_eq!(
        received.header("authorization"),
        Some("Bearer sk-test"),
        "api key should be sent as bearer auth"
    );
    assert_eq!(received.header("openai-organization"), Some("org-test"));
    assert_eq!(received.header("openai-project"), Some("proj-test"));

    let json: Value = serde_json::from_str(&received.body).expect("request body should be JSON");
    assert_eq!(json["model"], "debug-model");
    assert_eq!(json["stream"], true);
    assert_eq!(json["store"], false);
    assert_eq!(json["parallel_tool_calls"], false);
    assert!(json.get("previous_response_id").is_none());
    assert!(json.get("conversation").is_none());
}

#[ignore = "requires loopback TCP permission; default tests cover this behavior without network"]
#[tokio::test]
async fn stream_model_maps_authentication_status_to_model_error() {
    let server = TestServer::spawn(TestResponse::plain(401, "bad api key"));
    let error = expect_setup_error(
        provider(server.base_url())
            .stream_model(request(), ModelStreamContext::default())
            .await,
    );

    assert_eq!(error.kind(), ProviderErrorKind::Authentication);
}

#[ignore = "requires loopback TCP permission; default tests cover this behavior without network"]
#[tokio::test]
async fn stream_model_maps_rate_limit_status_to_model_error() {
    let server = TestServer::spawn(TestResponse::plain(429, "slow down"));
    let error = expect_setup_error(
        provider(server.base_url())
            .stream_model(request(), ModelStreamContext::default())
            .await,
    );

    assert_eq!(error.kind(), ProviderErrorKind::RateLimited);
}

#[ignore = "requires loopback TCP permission; default tests cover this behavior without network"]
#[tokio::test]
async fn stream_model_maps_server_status_to_unavailable() {
    let server = TestServer::spawn(TestResponse::plain(503, "try later"));
    let error = expect_setup_error(
        provider(server.base_url())
            .stream_model(request(), ModelStreamContext::default())
            .await,
    );

    assert_eq!(error.kind(), ProviderErrorKind::Unavailable);
}

#[ignore = "requires loopback TCP permission; default tests cover this behavior without network"]
#[tokio::test]
async fn stream_model_honors_pre_cancelled_context_before_sending_request() {
    let server = TestServer::spawn(
        TestResponse::ok_sse("data: [DONE]\n\n")
            .with_accept_timeout(std::time::Duration::from_millis(100)),
    );
    let token = CancellationToken::new();
    token.cancel();

    let error = expect_setup_error(
        provider(server.base_url())
            .stream_model(request(), ModelStreamContext::new(token))
            .await,
    );

    assert!(matches!(error, ModelError::Cancelled));
    assert_eq!(server.request_count(), 0);
}

#[ignore = "requires loopback TCP permission; default tests cover this behavior without network"]
#[tokio::test]
async fn stream_model_honors_cancellation_during_streaming() {
    let body = concat!(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hello\"}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\" world\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello world\"}]}],\"usage\":{\"input_tokens\":9,\"output_tokens\":3}}}\n\n",
    );
    let server = TestServer::spawn(TestResponse::ok_sse(body));
    let token = CancellationToken::new();
    let mut stream = provider(server.base_url())
        .stream_model(request(), ModelStreamContext::new(token.clone()))
        .await
        .expect("stream setup should succeed");

    assert_eq!(
        stream.try_next().await.expect("started event"),
        Some(ModelEvent::Started)
    );
    token.cancel();

    let error = stream
        .try_next()
        .await
        .expect_err("cancelled stream should emit cancellation error");
    assert!(matches!(error, ModelError::Cancelled));
}

#[ignore = "requires --features live-tests, MERRY_OPENAI_LIVE_TESTS=1, OPENAI_API_KEY, MERRY_OPENAI_MODEL, optional MERRY_OPENAI_BASE_URL, and --ignored"]
#[tokio::test]
async fn live_openai_responses_stream_smoke_test() {
    if !cfg!(feature = "live-tests") {
        return;
    }

    if std::env::var("MERRY_OPENAI_LIVE_TESTS").ok().as_deref() != Some("1") {
        return;
    }

    let api_key = match std::env::var("OPENAI_API_KEY") {
        Ok(api_key) => api_key,
        Err(_) => return,
    };
    let model = match std::env::var("MERRY_OPENAI_MODEL") {
        Ok(model) => model,
        Err(_) => return,
    };

    let mut config = OpenAiProviderConfig::new(&api_key).expect("valid api key");
    if let Ok(base_url) = std::env::var("MERRY_OPENAI_BASE_URL") {
        config = config.with_base_url(&base_url).expect("valid base url");
    }
    let provider = OpenAiProvider::new(config);
    let request = ModelRequest::new(
        ModelName::new(&model).expect("valid model name"),
        vec![
            ModelMessage::new(
                ModelMessageRole::User,
                ModelContent::text("Reply with one short sentence.").expect("valid content"),
            )
            .expect("valid message"),
        ],
        Vec::new(),
        GenerationConfig::new(Some(16), false).expect("valid generation config"),
    )
    .expect("valid request");

    let events = provider
        .stream_model(request, ModelStreamContext::default())
        .await
        .expect("live stream setup should succeed")
        .try_collect::<Vec<_>>()
        .await
        .expect("live stream should succeed");

    assert_eq!(events.first(), Some(&ModelEvent::Started));
    assert!(
        events.iter().any(|event| {
            matches!(
                event,
                ModelEvent::OutputTextDelta { .. } | ModelEvent::Completed { .. }
            )
        }),
        "live stream should emit text delta or completion"
    );
    assert!(matches!(events.last(), Some(ModelEvent::Completed { .. })));
}

#[derive(Debug)]
struct TestResponse {
    status: u16,
    content_type: &'static str,
    body: &'static str,
    accept_timeout: Option<std::time::Duration>,
}

impl TestResponse {
    fn ok_sse(body: &'static str) -> Self {
        Self {
            status: 200,
            content_type: "text/event-stream",
            body,
            accept_timeout: None,
        }
    }

    fn plain(status: u16, body: &'static str) -> Self {
        Self {
            status,
            content_type: "text/plain",
            body,
            accept_timeout: None,
        }
    }

    fn with_accept_timeout(mut self, accept_timeout: std::time::Duration) -> Self {
        self.accept_timeout = Some(accept_timeout);
        self
    }
}

#[derive(Debug)]
struct TestServer {
    base_url: String,
    handle: Option<thread::JoinHandle<Vec<ReceivedRequest>>>,
}

impl TestServer {
    fn spawn(response: TestResponse) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server should bind");
        let address = listener.local_addr().expect("local address should exist");
        if let Some(timeout) = response.accept_timeout {
            listener
                .set_nonblocking(true)
                .expect("listener should be nonblocking");
            let handle = thread::spawn(move || {
                let mut requests = Vec::new();
                let start = std::time::Instant::now();
                while start.elapsed() < timeout {
                    match listener.accept() {
                        Ok((mut stream, _address)) => {
                            let request = read_request(&mut stream);
                            write_response(&mut stream, &response);
                            requests.push(request);
                            break;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(error) => panic!("test server accept failed: {error}"),
                    }
                }
                requests
            });

            return Self {
                base_url: format!("http://{address}"),
                handle: Some(handle),
            };
        }

        listener
            .set_nonblocking(false)
            .expect("listener should be blocking");

        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            if let Ok((mut stream, _address)) = listener.accept() {
                let request = read_request(&mut stream);
                write_response(&mut stream, &response);
                requests.push(request);
            }
            requests
        });

        Self {
            base_url: format!("http://{address}"),
            handle: Some(handle),
        }
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn received(mut self) -> ReceivedRequest {
        let requests = self.take_requests();
        requests
            .into_iter()
            .next()
            .expect("server should receive a request")
    }

    fn request_count(mut self) -> usize {
        self.take_requests().len()
    }

    fn take_requests(&mut self) -> Vec<ReceivedRequest> {
        match self.handle.take() {
            Some(handle) => handle.join().expect("test server thread should join"),
            None => Vec::new(),
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = TcpStream::connect(
                self.base_url
                    .strip_prefix("http://")
                    .expect("test base URL should be http"),
            );
            let _ = handle.join();
        }
    }
}

#[derive(Debug)]
struct ReceivedRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: String,
}

impl ReceivedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

fn read_request(stream: &mut TcpStream) -> ReceivedRequest {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    let header_end = loop {
        let read = stream.read(&mut chunk).expect("request should be readable");
        if read == 0 {
            panic!("connection closed before headers");
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = find_header_end(&buffer) {
            break position;
        }
    };

    let header_bytes = &buffer[..header_end];
    let header_text = String::from_utf8(header_bytes.to_vec()).expect("headers should be UTF-8");
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().expect("request line should exist");
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .expect("method should exist")
        .to_owned();
    let path = request_parts.next().expect("path should exist").to_owned();
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.to_ascii_lowercase(), value.trim().to_owned()))
        })
        .collect::<BTreeMap<_, _>>();

    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let body_start = header_end + 4;
    while buffer.len() < body_start + content_length {
        let read = stream.read(&mut chunk).expect("body should be readable");
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }

    let body = String::from_utf8(buffer[body_start..body_start + content_length].to_vec())
        .expect("body should be UTF-8");

    ReceivedRequest {
        method,
        path,
        headers,
        body,
    }
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn write_response(stream: &mut TcpStream, response: &TestResponse) {
    let status_text = match response.status {
        200 => "OK",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let headers = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        response.status,
        status_text,
        response.content_type,
        response.body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .expect("headers should write");
    stream
        .write_all(response.body.as_bytes())
        .expect("body should write");
}
