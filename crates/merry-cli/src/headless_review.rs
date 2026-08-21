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

    pub(crate) fn start(self) -> HeadlessPermissionReviewTask {
        let Self {
            requests,
            cancellation,
            ..
        } = self;
        let task_cancellation = cancellation.clone();
        let handle = tokio::spawn(async move {
            run_headless_permission_review(requests, task_cancellation).await
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

async fn run_headless_permission_review(
    mut requests: mpsc::Receiver<PermissionReviewRequest>,
    cancellation: CancellationToken,
) -> io::Result<()> {
    let mut input = BufReader::new(tokio::io::stdin());
    let mut output = BufWriter::new(tokio::io::stderr());

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
        review_request(request, &mut input, &mut output, &cancellation).await?;
    }
}

async fn review_request<R, W>(
    request: PermissionReviewRequest,
    input: &mut R,
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

    match read_headless_decision(input, output, cancellation).await? {
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
    use super::{HeadlessDecision, parse_decision, read_headless_decision};
    use std::io::Cursor;
    use tokio::io::BufReader;
    use tokio_util::sync::CancellationToken;

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
}
