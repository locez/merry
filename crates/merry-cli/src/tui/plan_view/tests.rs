use crate::tui::{
    controller::{
        ControllerEffect, handle_key_action, handle_key_event, handle_mouse_input,
        handle_mouse_scroll_down_by, handle_mouse_scroll_up_by,
    },
    keymap::{KeyAction, Keymap},
    overlay::Overlay,
    projector::TuiProjector,
    render::{render_to_buffer, render_to_text},
    state::{TimelineItem, TuiState},
    text_interaction::MouseInput,
    theme::TuiTheme,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use merry_core::{
    PlanDocument, PlanDocumentStatus, PlanDocumentSummary, PlanId, PlanModeState, RuntimeEvent,
    RuntimeEventSource, SessionId, TodoItem, TodoStatus,
};
use ratatui::layout::{Position, Size};

fn test_state() -> TuiState {
    TuiState::new(
        "/repo".into(),
        "test/model".to_owned(),
        Keymap::default(),
        TuiTheme::default(),
    )
}

fn source() -> RuntimeEventSource {
    RuntimeEventSource::new(SessionId::new("plan-tui-test").expect("valid id"), 1)
}

fn plan_doc(id: &str, title: &str, file_name: &str, content: &str) -> PlanDocument {
    PlanDocument {
        summary: PlanDocumentSummary {
            id: PlanId::new(id).expect("valid plan id"),
            title: title.to_owned(),
            file_name: file_name.to_owned(),
            status: PlanDocumentStatus::AwaitingApproval,
            revision: 1,
            created_at_ms: 10,
            updated_at_ms: 20,
        },
        content: content.to_owned(),
    }
}

fn find_text_position(buffer: &ratatui::buffer::Buffer, needle: &str) -> Option<(u16, u16)> {
    let area = buffer.area;
    for y in area.y..area.y + area.height {
        let mut row = String::new();
        let mut cols = Vec::new();
        for x in area.x..area.x + area.width {
            let sym = buffer[(x, y)].symbol();
            let start = row.len();
            row.push_str(sym);
            cols.push((start, x));
        }
        if let Some(byte_idx) = row.find(needle)
            && let Some(&(_, col)) = cols.iter().find(|&&(start, _)| start == byte_idx)
        {
            return Some((col, y));
        }
    }
    None
}

#[test]
fn plan_view_renders_markdown_multi_plan_rail_and_inline_alt_c_comments() {
    let mut state = test_state();
    let mut projector = TuiProjector::default();
    let plan1 = plan_doc(
        "plan-1",
        "Auth Refactor",
        "001-auth-refactor.md",
        "# Auth Refactor\n\nReplace legacy session cookie parsing with signed tokens.",
    );
    let plan2 = plan_doc(
        "plan-2",
        "Database Migration",
        "002-database-migration.md",
        "# Database Migration\n\nAdd index on sessions table before backfilling rows.",
    );

    projector.apply(
        RuntimeEvent::PlanSaved {
            plan: plan1,
            source: source(),
        },
        &mut state,
    );
    projector.apply(
        RuntimeEvent::PlanApprovalRequested {
            plan: plan2,
            source: source(),
        },
        &mut state,
    );

    // Auto-opened on plan-2
    assert!(matches!(state.overlay(), Some(Overlay::PlanView(_))));
    let size = Size::new(110, 28);
    let buf = render_to_buffer(&state, size.width, size.height);
    let text = render_to_text(&state, size.width, size.height);
    assert!(
        text.contains("PLANS"),
        "multi-plan rail should render:\n{text}"
    );
    assert!(text.contains("Auth Refactor"), "{text}");
    assert!(text.contains("Database Migration"), "{text}");
    assert!(text.contains("Add index on sessions table"), "{text}");
    assert!(text.contains("[Approve]"), "{text}");
    assert!(text.contains("[Request changes]"), "{text}");

    // Drag-select text inside the plan body to add an inline comment (Alt+C mode is active by default).
    let (start_x, start_y) = find_text_position(&buf, "Add index").expect("quote start");
    let end_x = start_x + 8; // "Add index"
    assert_eq!(
        handle_mouse_input(
            MouseInput::Down(Position::new(start_x, start_y)),
            size,
            &mut state
        ),
        ControllerEffect::None
    );
    assert_eq!(
        handle_mouse_input(
            MouseInput::Drag(Position::new(end_x, start_y)),
            size,
            &mut state
        ),
        ControllerEffect::None
    );
    assert_eq!(
        handle_mouse_input(
            MouseInput::Up(Position::new(end_x, start_y)),
            size,
            &mut state
        ),
        ControllerEffect::None
    );

    // Pending quote captured in child_state; type note and press Enter to pin it.
    for ch in "use concurrent index".chars() {
        handle_key_event(
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            &mut state,
        );
    }
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut state
        ),
        ControllerEffect::None
    );
    let after_pin = render_to_text(&state, size.width, size.height);
    assert!(
        after_pin.contains("[Approve w/ 1 comments]"),
        "footer should reflect pinned inline comment:\n{after_pin}"
    );

    // Pressing 'a' approves with the formatted inline comments attached!
    let effect = handle_key_event(
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        &mut state,
    );
    match effect {
        ControllerEffect::ApprovePlan { plan_id, note } => {
            assert_eq!(plan_id.as_str(), "plan-2");
            let note = note.expect("inline comment note should be attached");
            assert!(note.contains("Review comments:"), "{note}");
            assert!(note.contains("> Add index"), "{note}");
            assert!(note.contains("use concurrent index"), "{note}");
        }
        other => panic!("expected ApprovePlan with note, got {other:?}"),
    }
    assert!(state.overlay().is_none());
}

#[test]
fn shift_tab_and_slash_plan_toggle_plan_mode_and_status_chips() {
    let mut state = test_state();
    assert_eq!(
        handle_key_event(
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
            &mut state
        ),
        ControllerEffect::SetPlanMode {
            enabled: true,
            initial_prompt: None,
        }
    );

    state.insert_input_str("/plan build todo dock");
    assert_eq!(
        handle_key_action(KeyAction::Submit, &mut state),
        ControllerEffect::SetPlanMode {
            enabled: true,
            initial_prompt: Some("build todo dock".to_owned()),
        }
    );

    state.plan_mut().set_mode(PlanModeState::Active);
    let rendered = render_to_text(&state, 100, 20);
    assert!(
        rendered.contains("PLAN"),
        "status bar should show PLAN chip:\n{rendered}"
    );
    assert!(
        rendered.contains("Plan Mode (read-only)"),
        "composer placeholder should show Plan Mode hint:\n{rendered}"
    );
}

#[test]
fn todo_dock_collapses_expands_and_scrolls_above_timeline() {
    let mut state = test_state();
    state.push_timeline_item(TimelineItem::User {
        text: "USER_PROMPT".to_owned(),
    });
    state.push_timeline_item(TimelineItem::Assistant {
        text: "Working through checklist.".to_owned(),
    });

    let size = Size::new(100, 24);
    assert!(!render_to_text(&state, size.width, size.height).contains("[Todos"));

    let mut projector = TuiProjector::default();
    let items: Vec<TodoItem> = (1..=7)
        .map(|i| TodoItem {
            id: format!("t{i}"),
            content: format!("Task item {i:02}"),
            status: match i {
                1 | 2 => TodoStatus::Completed,
                3 => TodoStatus::InProgress,
                _ => TodoStatus::Pending,
            },
        })
        .collect();
    projector.apply(
        RuntimeEvent::TodoListUpdated {
            items,
            source: source(),
        },
        &mut state,
    );

    let collapsed_buf = render_to_buffer(&state, size.width, size.height);
    let collapsed_text = render_to_text(&state, size.width, size.height);
    assert!(collapsed_text.contains("[Todos 2/7 ▸]"), "{collapsed_text}");
    assert!(collapsed_text.contains("Task item 03"), "{collapsed_text}");

    let (dock_x, dock_y) = find_text_position(&collapsed_buf, "Task item 03").expect("todo dock");
    assert_eq!(
        handle_mouse_input(
            MouseInput::Down(Position::new(dock_x, dock_y)),
            size,
            &mut state
        ),
        ControllerEffect::None
    );
    assert!(state.todo_dock().is_expanded());
    let expanded_buf = render_to_buffer(&state, size.width, size.height);
    let expanded_text = render_to_text(&state, size.width, size.height);
    assert!(expanded_text.contains("1–5/7"), "{expanded_text}");
    assert!(expanded_text.contains("Task item 01"), "{expanded_text}");
    assert!(expanded_text.contains("Task item 05"), "{expanded_text}");

    let (row_x, row_y) = find_text_position(&expanded_buf, "Task item 01").expect("todo row");
    handle_mouse_scroll_down_by(Position::new(row_x, row_y), size, 1, &mut state);
    assert_eq!(state.todo_dock().scroll(), 1);
}

#[test]
fn plan_view_wraps_compact_footer_actions_and_hit_tests_on_mobile_narrow_screens() {
    let mut state = test_state();
    let mut projector = TuiProjector::default();
    let plan = plan_doc(
        "plan-mobile",
        "Mobile Responsive Plan Review",
        "001-mobile-responsive-plan-review.md",
        "# Mobile Plan\n\nVerify all actions stay visible on a 40-column phone terminal.",
    );
    projector.apply(
        RuntimeEvent::PlanApprovalRequested {
            plan,
            source: source(),
        },
        &mut state,
    );

    let size = Size::new(40, 24);
    let buf = render_to_buffer(&state, size.width, size.height);
    let text = render_to_text(&state, size.width, size.height);
    for expected in [
        "[Approve]",
        "[Revise]",
        "[Abandon]",
        "[Comment ●]",
        "[Copy]",
        "[Back]",
    ] {
        assert!(
            text.contains(expected),
            "narrow mobile screen (40x24) should show {expected}:\n{text}"
        );
    }

    // Verify wrapped second-row chip hit-testing works accurately: click [Copy] on row 2.
    let (copy_x, copy_y) = find_text_position(&buf, "[Copy]").expect("Copy chip on wrapped row");
    let (approve_x, approve_y) = find_text_position(&buf, "[Approve]").expect("Approve chip");
    assert!(
        copy_y > approve_y,
        "secondary actions should wrap onto a lower footer row at 40 cols (approve_y={approve_y}, copy_y={copy_y})"
    );
    match handle_mouse_input(
        MouseInput::Down(Position::new(copy_x + 1, copy_y)),
        size,
        &mut state,
    ) {
        ControllerEffect::CopyText(copied) => {
            assert!(copied.contains("# Mobile Plan"), "{copied}");
        }
        other => panic!("expected CopyText from wrapped [Copy] chip, got {other:?}"),
    }

    // Click [Approve] on row 1.
    match handle_mouse_input(
        MouseInput::Down(Position::new(approve_x + 1, approve_y)),
        size,
        &mut state,
    ) {
        ControllerEffect::ApprovePlan { plan_id, .. } => {
            assert_eq!(plan_id.as_str(), "plan-mobile");
        }
        other => panic!("expected ApprovePlan from [Approve] chip, got {other:?}"),
    }
}

#[test]
fn plan_view_left_rail_and_right_body_both_scroll_cleanly() {
    let mut state = test_state();
    let mut projector = TuiProjector::default();
    let long_body = (1..=40)
        .map(|line| format!("- Detailed step line {line:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    for index in 1..=16 {
        let doc = plan_doc(
            &format!("plan-{index:02}"),
            &format!("Plan Item {index:02}"),
            &format!("{index:03}-plan-item.md"),
            &format!("# Plan Item {index:02}\n\n{long_body}\n\nTAIL_MARKER_{index:02}"),
        );
        projector.apply(
            if index == 16 {
                RuntimeEvent::PlanApprovalRequested {
                    plan: doc,
                    source: source(),
                }
            } else {
                RuntimeEvent::PlanSaved {
                    plan: doc,
                    source: source(),
                }
            },
            &mut state,
        );
    }

    // Height 16 gives inner height 14 -> main height 12 -> rail shows header + 11 visible rows.
    let size = Size::new(110, 16);
    crate::tui::render::prepare_viewport(&mut state, size);
    let initial_buf = render_to_buffer(&state, size.width, size.height);
    let initial_text = render_to_text(&state, size.width, size.height);
    assert!(
        initial_text.contains("PLANS 6-16/16") && initial_text.contains("Plan Item 16"),
        "latest selected plan should auto-scroll into view:\n{initial_text}"
    );
    assert!(
        initial_text.contains("Detailed step line 01") && !initial_text.contains("TAIL_MARKER_16"),
        "plan body should open at the top:\n{initial_text}"
    );

    // 1. Scroll DOWN over the right-hand plan Markdown body: must move immediately on first tick!
    let (body_x, body_y) = find_text_position(&initial_buf, "Detailed step line 01").expect("body");
    handle_mouse_scroll_down_by(Position::new(body_x, body_y), size, 3, &mut state);
    let after_one_tick = render_to_text(&state, size.width, size.height);
    assert!(
        !after_one_tick.contains("Plan Item 16\n")
            && after_one_tick.contains("Detailed step line 05"),
        "first mouse wheel tick down must scroll immediately:\n{after_one_tick}"
    );
    handle_mouse_scroll_down_by(Position::new(body_x, body_y), size, 50, &mut state);
    let body_scrolled_down = render_to_text(&state, size.width, size.height);
    assert!(
        body_scrolled_down.contains("TAIL_MARKER_16")
            && !body_scrolled_down.contains("Detailed step line 01"),
        "scrolling down over right plan body should reveal tail:\n{body_scrolled_down}"
    );

    // Scroll UP over the right-hand plan body: returns to top.
    handle_mouse_scroll_up_by(Position::new(body_x, body_y), size, 60, &mut state);
    let body_scrolled_up = render_to_text(&state, size.width, size.height);
    assert!(
        body_scrolled_up.contains("Detailed step line 01"),
        "scrolling up over right plan body should return to top:\n{body_scrolled_up}"
    );

    // 2. Scroll UP over the left rail: switches to previous plans and scrolls the rail window up.
    let (rail_x, rail_y) = find_text_position(&initial_buf, "Plan Item 06").expect("rail row");
    for _ in 0..15 {
        handle_mouse_scroll_up_by(Position::new(rail_x, rail_y), size, 3, &mut state);
    }
    let rail_top_buf = render_to_buffer(&state, size.width, size.height);
    let rail_top_text = render_to_text(&state, size.width, size.height);
    assert!(
        rail_top_text.contains("PLANS 1-11/16") && rail_top_text.contains("Plan Item 01"),
        "scrolling up over left rail should step selection to Plan Item 01:\n{rail_top_text}"
    );
    assert!(
        !rail_top_text.contains("Plan Item 15"),
        "bottom rail plans should scroll out when at top of rail:\n{rail_top_text}"
    );

    // Scroll DOWN over the left rail twice -> selects Plan Item 03.
    handle_mouse_scroll_down_by(Position::new(rail_x, rail_y), size, 3, &mut state);
    handle_mouse_scroll_down_by(Position::new(rail_x, rail_y), size, 3, &mut state);
    let after_down = render_to_text(&state, size.width, size.height);
    assert!(
        after_down.contains("Plan · Plan Item 03"),
        "scrolling down twice over left rail should select Plan Item 03:\n{after_down}"
    );

    // Clicking a visible rail item also selects it directly.
    let (item5_x, item5_y) =
        find_text_position(&rail_top_buf, "Plan Item 05").expect("Plan Item 05 in rail");
    handle_mouse_input(
        MouseInput::Down(Position::new(item5_x, item5_y)),
        size,
        &mut state,
    );
    let after_click = render_to_text(&state, size.width, size.height);
    assert!(
        after_click.contains("Plan · Plan Item 05"),
        "clicking rail row should open exact plan:\n{after_click}"
    );
}
