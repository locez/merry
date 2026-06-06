use super::*;

#[tokio::test(flavor = "current_thread")]
async fn cancelled_event_send_returns_false_when_channel_is_closed() {
    let inner = runtime_inner();
    let (sender, receiver) = mpsc::channel(1);
    drop(receiver);

    let sent = send_cancelled_event(&inner, &sender).await;
    let projection = {
        let session = inner.session.lock().await;
        session.ledger_projection()
    };

    assert!(!sent);
    assert!(projection.entries().is_empty());
}
