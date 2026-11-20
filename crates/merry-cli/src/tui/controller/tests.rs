use super::{
    CoalescedTerminalEvent, TUI_REFRESH_INTERVAL, coalesce_terminal_events, drain_background_tasks,
    new_refresh_interval,
};
use crate::tui::{terminal::TerminalEvent, text_interaction::MouseInput};
use ratatui::layout::Position;
use tokio::{sync::oneshot, task::JoinSet, time};

mod selection_scroll;

#[test]
fn coalesces_wheel_bursts_into_single_notch_without_amplification() {
    let pos = Position::new(10, 5);
    let burst = vec![
        TerminalEvent::MouseScrollUp(pos),
        TerminalEvent::MouseScrollUp(pos),
        TerminalEvent::MouseScrollUp(pos),
    ];
    assert_eq!(
        coalesce_terminal_events(burst, 3),
        vec![CoalescedTerminalEvent::MouseScroll {
            position: pos,
            up: true,
            lines: 3,
        }]
    );

    let four_events = vec![
        TerminalEvent::MouseScrollUp(pos),
        TerminalEvent::MouseScrollUp(pos),
        TerminalEvent::MouseScrollUp(pos),
        TerminalEvent::MouseScrollUp(pos),
    ];
    assert_eq!(
        coalesce_terminal_events(four_events, 3),
        vec![CoalescedTerminalEvent::MouseScroll {
            position: pos,
            up: true,
            lines: 4,
        }]
    );

    let cancelling = vec![
        TerminalEvent::MouseScrollUp(pos),
        TerminalEvent::MouseScrollDown(pos),
    ];
    assert!(coalesce_terminal_events(cancelling, 3).is_empty());
}

#[test]
fn coalesces_consecutive_mouse_drags_and_resizes_while_preserving_barriers() {
    let events = vec![
        TerminalEvent::Mouse(MouseInput::Down(Position::new(1, 2))),
        TerminalEvent::Mouse(MouseInput::Drag(Position::new(2, 2))),
        TerminalEvent::Mouse(MouseInput::Drag(Position::new(5, 3))),
        TerminalEvent::Mouse(MouseInput::Drag(Position::new(9, 4))),
        TerminalEvent::Mouse(MouseInput::Up(Position::new(9, 4))),
        TerminalEvent::Resize,
        TerminalEvent::Resize,
        TerminalEvent::Resize,
    ];
    assert_eq!(
        coalesce_terminal_events(events, 3),
        vec![
            CoalescedTerminalEvent::Event(TerminalEvent::Mouse(MouseInput::Down(Position::new(
                1, 2
            )))),
            CoalescedTerminalEvent::Event(TerminalEvent::Mouse(MouseInput::Drag(Position::new(
                9, 4
            )))),
            CoalescedTerminalEvent::Event(TerminalEvent::Mouse(MouseInput::Up(Position::new(
                9, 4
            )))),
            CoalescedTerminalEvent::Event(TerminalEvent::Resize),
        ]
    );
}

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
