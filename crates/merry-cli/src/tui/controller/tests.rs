use super::{TUI_REFRESH_INTERVAL, drain_background_tasks, new_refresh_interval};
use tokio::{sync::oneshot, task::JoinSet, time};

mod selection_scroll;

#[tokio::test(start_paused = true)]
async fn refresh_interval_is_not_reset_by_unrelated_work() {
    let mut refresh_interval = new_refresh_interval();
    let first_tick = refresh_interval.tick().await;
    for _ in 0..8 {
        time::advance(TUI_REFRESH_INTERVAL / 5).await;
    }
    assert_eq!(
        refresh_interval.tick().await,
        first_tick + TUI_REFRESH_INTERVAL
    );
}

#[tokio::test]
async fn background_shutdown_awaits_completion_and_can_repeat() {
    let mut tasks = JoinSet::new();
    let (sender, mut receiver) = oneshot::channel();
    tasks.spawn(async move {
        tokio::task::yield_now().await;
        sender.send(()).expect("completion remains observed");
    });
    drain_background_tasks(&mut tasks)
        .await
        .expect("task settles");
    assert_eq!(receiver.try_recv(), Ok(()));
    drain_background_tasks(&mut tasks)
        .await
        .expect("repeated shutdown is harmless");
}

#[tokio::test]
async fn background_shutdown_reports_failure_after_awaiting_remaining_tasks() {
    let mut tasks = JoinSet::new();
    let (sender, mut receiver) = oneshot::channel();
    tasks.spawn(async { panic!("injected background task failure") });
    tasks.spawn(async move {
        tokio::task::yield_now().await;
        sender.send(()).expect("completion remains observed");
    });
    assert!(drain_background_tasks(&mut tasks).await.is_err());
    assert_eq!(receiver.try_recv(), Ok(()));
}
