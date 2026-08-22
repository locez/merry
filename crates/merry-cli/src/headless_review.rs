use merry_runtime::{
    ChannelPermissionAdmissionSource, PermissionReviewRequest, RequestedCapability,
};
use std::{io, sync::Arc};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter},
    sync::mpsc,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

/// Where a headless run reads permission-review answers from.
///
/// `merry run -` reads the task from stdin to end-of-file, so stdin can no
/// longer carry answers: reusing it would read that end-of-file and silently
/// deny every request. Those runs answer on the controlling terminal instead,
/// and deny with an explicit reason when the process has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewInputChannel {
    Stdin,
    ControllingTerminal,
}

/// Reason recorded when a run has no channel to answer permission review on.
const NO_REVIEW_INPUT_REASON: &str =
    "headless permission review had no input channel; defaulting to deny";

/// Operator-facing note explaining why a request was denied unanswered.
const NO_REVIEW_INPUT_NOTICE: &str = concat!(
    "\nNo input channel for permission review: the task was read from stdin and ",
    "no controlling terminal is available. Denying.\n"
);

pub(crate) struct HeadlessPermissionReviewer {
    source: Arc<ChannelPermissionAdmissionSource>,
    requests: mpsc::Receiver<PermissionReviewRequest>,
    cancellation: CancellationToken,
}

pub(crate) struct HeadlessPermissionReviewTask {
    cancellation: CancellationToken,
    handle: JoinHandle<io::Result<()>>,
}

impl HeadlessPermissionReviewer {
    pub(crate) fn new() -> Self {
        let (source, requests) = ChannelPermissionAdmissionSource::channel(8);
        Self {
            source: Arc::new(source),
            requests,
            cancellation: CancellationToken::new(),
        }
    }

    pub(crate) fn source(&self) -> Arc<ChannelPermissionAdmissionSource> {
        Arc::clone(&self.source)
    }

    pub(crate) fn start(self, channel: ReviewInputChannel) -> HeadlessPermissionReviewTask {
        let Self {
            requests,
            cancellation,
            ..
        } = self;
        let task_cancellation = cancellation.clone();
        let handle = tokio::spawn(async move {
            let input = open_review_input(channel).await;
            run_headless_permission_review(requests, input, tokio::io::stderr(), task_cancellation)
                .await
        });
        HeadlessPermissionReviewTask {
            cancellation,
            handle,
        }
    }
}

impl HeadlessPermissionReviewTask {
    pub(crate) async fn finish(self) -> io::Result<()> {
        self.cancellation.cancel();
        match self.handle.await {
            Ok(result) => result,
            Err(error) => Err(io::Error::other(format!(
                "headless permission review task failed: {error}"
            ))),
        }
    }
}

/// Opens the reader that answers permission reviews for this run.
///
/// A run whose task came from stdin has already consumed that stream, so it
/// falls back to the controlling terminal. `None` means review has no way to
/// ask, which is reported per request rather than silently defaulting to deny.
async fn open_review_input(
    channel: ReviewInputChannel,
) -> Option<Box<dyn AsyncBufRead + Unpin + Send>> {
    match channel {
        ReviewInputChannel::Stdin => Some(Box::new(BufReader::new(tokio::io::stdin()))),
        ReviewInputChannel::ControllingTerminal => tokio::fs::File::open("/dev/tty")
            .await
            .ok()
            .map(|terminal| {
                Box::new(BufReader::new(terminal)) as Box<dyn AsyncBufRead + Unpin + Send>
            }),
    }
}

async fn run_headless_permission_review<R, W>(
    mut requests: mpsc::Receiver<PermissionReviewRequest>,
    mut input: Option<R>,
    output: W,
    cancellation: CancellationToken,
) -> io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut output = BufWriter::new(output);

    loop {
        let Some(request) = (tokio::select! {
            biased;
            () = cancellation.cancelled() => None,
            request = requests.recv() => request,
        }) else {
            return Ok(());
        };

        if request.is_cancelled() {
            continue;
        }
        review_request(request, input.as_mut(), &mut output, &cancellation).await?;
    }
}

async fn review_request<R, W>(
    request: PermissionReviewRequest,
    input: Option<&mut R>,
    output: &mut W,
    cancellation: &CancellationToken,
) -> io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    output
        .write_all(format_permission_prompt(&request).as_bytes())
        .await?;
    output.flush().await?;

    let decision = match input {
        Some(input) => read_headless_decision(input, output, cancellation).await?,
        None => HeadlessDecision::NoInputChannel,
    };
    match decision {
        HeadlessDecision::Approve => request
            .approve("approved by headless command-line review")
            .map_err(|error| {
                io::Error::other(format!(
                    "failed to approve headless permission review: {error}"
                ))
            }),
        HeadlessDecision::Deny => request
            .deny("denied by headless command-line review")
            .map_err(|error| {
                io::Error::other(format!(
                    "failed to deny headless permission review: {error}"
                ))
            }),
        HeadlessDecision::InputEnded => request
            .deny("headless permission review input ended; defaulting to deny")
            .map_err(|error| {
                io::Error::other(format!(
                    "failed to deny headless permission review at input EOF: {error}"
                ))
            }),
        HeadlessDecision::NoInputChannel => {
            output.write_all(NO_REVIEW_INPUT_NOTICE.as_bytes()).await?;
            output.flush().await?;
            request.deny(NO_REVIEW_INPUT_REASON).map_err(|error| {
                io::Error::other(format!(
                    "failed to deny headless permission review without an input channel: {error}"
                ))
            })
        }
        HeadlessDecision::Cancelled => {
            let _ = request.deny("headless permission review was cancelled");
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeadlessDecision {
    Approve,
    Deny,
    InputEnded,
    NoInputChannel,
    Cancelled,
}

async fn read_headless_decision<R, W>(
    input: &mut R,
    output: &mut W,
    cancellation: &CancellationToken,
) -> io::Result<HeadlessDecision>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut line = String::new();
    loop {
        line.clear();
        let bytes_read = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Ok(HeadlessDecision::Cancelled),
            result = input.read_line(&mut line) => result?,
        };

        if bytes_read == 0 {
            return Ok(HeadlessDecision::InputEnded);
        }

        match parse_decision(&line) {
            Some(true) => return Ok(HeadlessDecision::Approve),
            Some(false) => return Ok(HeadlessDecision::Deny),
            None => {
                output.write_all(b"Please answer yes or no [y/N]: ").await?;
                output.flush().await?;
            }
        }
    }
}

fn parse_decision(value: &str) -> Option<bool> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("n") || value.eq_ignore_ascii_case("no") {
        Some(false)
    } else if value.eq_ignore_ascii_case("y") || value.eq_ignore_ascii_case("yes") {
        Some(true)
    } else {
        None
    }
}

fn format_permission_prompt(request: &PermissionReviewRequest) -> String {
    let permission_request = request.request();
    let mut lines = vec![format!(
        "Permission review required ({})",
        request.approval_id()
    )];
    if let Some(failure) = request.review_failure() {
        lines.push(format!("AI review unavailable: {failure}"));
    }
    if let Some(reason) = permission_request.reason() {
        lines.push(format!("reason: {reason}"));
    }
    if permission_request.is_action_review() {
        lines.push("review: high-risk process action".to_owned());
    }
    lines.push(format!("action: {}", permission_request.action().summary()));
    for capability in permission_request.requested() {
        let line = match capability {
            RequestedCapability::Network => "requested: network".to_owned(),
            RequestedCapability::Path(path) => {
                format!(
                    "requested: path {} ({})",
                    path.path(),
                    path.access().as_str()
                )
            }
            RequestedCapability::HostIntegration(integration) => {
                format!("requested: host integration {}", integration.as_str())
            }
        };
        lines.push(line);
    }
    lines.push("Allow? [y/N] ".to_owned());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{
        HeadlessDecision, NO_REVIEW_INPUT_REASON, parse_decision, read_headless_decision,
        review_request,
    };
    use merry_core::{PendingToolCall, ToolCallArguments, ToolCallId, ToolName};
    use merry_runtime::{
        ChannelPermissionAdmissionSource, PermissionAdmissionContext, PermissionAdmissionDecision,
        PermissionAdmissionSource, PermissionRequest, parse_permission_request,
    };
    use std::io::Cursor;
    use std::sync::Arc;
    use tokio::io::BufReader;
    use tokio_util::sync::CancellationToken;

    /// Reader type the no-input-channel case never constructs.
    type UnusedReader = BufReader<Cursor<Vec<u8>>>;

    fn network_permission_request() -> PermissionRequest {
        let call = PendingToolCall::new(
            ToolCallId::new("call-headless-review").expect("valid call id"),
            ToolName::new("request_permissions").expect("valid tool name"),
            ToolCallArguments::try_from(serde_json::json!({
                "requested": { "network": true },
                "for_action": { "command": "curl https://example.invalid", "cwd": null }
            }))
            .expect("valid tool call arguments"),
        );
        parse_permission_request(&call).expect("the request should parse")
    }

    #[test]
    fn command_line_decision_defaults_to_deny() {
        assert_eq!(parse_decision(""), Some(false));
        assert_eq!(parse_decision("n"), Some(false));
        assert_eq!(parse_decision("NO"), Some(false));
        assert_eq!(parse_decision("y"), Some(true));
        assert_eq!(parse_decision("Yes"), Some(true));
        assert_eq!(parse_decision("maybe"), None);
    }

    #[tokio::test]
    async fn command_line_review_reprompts_invalid_input_then_accepts() {
        let cancellation = CancellationToken::new();
        let mut input = BufReader::new(Cursor::new(b"maybe\nyes\n".to_vec()));
        let mut output = Vec::new();

        let decision = read_headless_decision(&mut input, &mut output, &cancellation)
            .await
            .expect("headless input should be readable");

        assert_eq!(decision, HeadlessDecision::Approve);
        assert_eq!(
            String::from_utf8(output).expect("prompt output should be UTF-8"),
            "Please answer yes or no [y/N]: "
        );
    }

    #[tokio::test]
    async fn command_line_review_defaults_to_deny_at_eof_and_honors_cancellation() {
        let cancellation = CancellationToken::new();
        let mut input = BufReader::new(Cursor::new(Vec::<u8>::new()));
        let mut output = Vec::new();
        assert_eq!(
            read_headless_decision(&mut input, &mut output, &cancellation)
                .await
                .expect("EOF should be handled"),
            HeadlessDecision::InputEnded
        );

        cancellation.cancel();
        let mut input = BufReader::new(Cursor::new(b"yes\n".to_vec()));
        assert_eq!(
            read_headless_decision(&mut input, &mut output, &cancellation)
                .await
                .expect("cancellation should be handled"),
            HeadlessDecision::Cancelled
        );
    }

    #[tokio::test]
    async fn review_without_an_input_channel_denies_and_reports_why() {
        let (source, mut requests) = ChannelPermissionAdmissionSource::channel(1);
        let source = Arc::new(source);
        let cancellation = CancellationToken::new();
        let review = {
            let source = Arc::clone(&source);
            let cancellation = cancellation.clone();
            tokio::spawn(async move {
                source
                    .review(
                        network_permission_request(),
                        PermissionAdmissionContext::new(cancellation),
                    )
                    .await
            })
        };

        let request = requests.recv().await.expect("the request should arrive");
        let mut output = Vec::new();
        review_request(
            request,
            None::<&mut UnusedReader>,
            &mut output,
            &cancellation,
        )
        .await
        .expect("review without an input channel should settle the request");

        let decision = review
            .await
            .expect("review task should join")
            .expect("review should resolve");
        match decision {
            PermissionAdmissionDecision::Denied(review) => {
                assert_eq!(review.rationale(), NO_REVIEW_INPUT_REASON);
            }
            PermissionAdmissionDecision::Approved(review) => panic!(
                "an unanswerable request must not be approved: {}",
                review.rationale()
            ),
        }
        let prompt = String::from_utf8(output).expect("prompt output should be UTF-8");
        assert!(
            prompt.contains("No input channel for permission review"),
            "the operator should be told why the request was denied unanswered: {prompt}"
        );
    }

    #[tokio::test]
    async fn review_over_an_available_channel_still_approves() {
        let (source, mut requests) = ChannelPermissionAdmissionSource::channel(1);
        let source = Arc::new(source);
        let cancellation = CancellationToken::new();
        let review = {
            let source = Arc::clone(&source);
            let cancellation = cancellation.clone();
            tokio::spawn(async move {
                source
                    .review(
                        network_permission_request(),
                        PermissionAdmissionContext::new(cancellation),
                    )
                    .await
            })
        };

        let request = requests.recv().await.expect("the request should arrive");
        let mut input = BufReader::new(Cursor::new(b"yes\n".to_vec()));
        let mut output = Vec::new();
        review_request(request, Some(&mut input), &mut output, &cancellation)
            .await
            .expect("an answered review should settle the request");

        let decision = review
            .await
            .expect("review task should join")
            .expect("review should resolve");
        assert!(
            matches!(decision, PermissionAdmissionDecision::Approved(_)),
            "an answered request should be approved: {decision:?}"
        );
    }
}
