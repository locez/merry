use crate::runtime::{send_cancelled_event, tests::support::common::runtime_inner};
use tokio::sync::mpsc;

#[tokio::test(flavor = "current_thread")]
async fn cancelled_event_send_returns_false_when_channel_is_closed() {
    let inner = runtime_inner();
    let (sender, receiver) = mpsc::channel(1);
    drop(receiver);

    let sent = send_cancelled_event(&inner, &sender).await;
    let next_sequence = inner.session.lock().await.next_sequence();

    assert!(!sent);
    assert_eq!(next_sequence, 0);
}
