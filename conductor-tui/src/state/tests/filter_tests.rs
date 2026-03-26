use super::*;

#[test]
fn show_closed_tickets_defaults_to_false() {
    let state = AppState::new();
    assert!(!state.show_closed_tickets);
}

#[test]
fn show_closed_tickets_toggle() {
    let mut state = AppState::new();
    assert!(!state.show_closed_tickets);
    state.show_closed_tickets = true;
    assert!(state.show_closed_tickets);
    state.show_closed_tickets = false;
    assert!(!state.show_closed_tickets);
}

#[test]
fn rebuild_filtered_tickets_hides_closed() {
    let mut state = AppState::new();
    state.data.tickets = vec![
        make_ticket("1", "open"),
        make_ticket("2", "closed"),
        make_ticket("3", "open"),
    ];
    state.show_closed_tickets = false;
    state.rebuild_filtered_tickets();
    assert_eq!(state.filtered_tickets.len(), 2);
    assert!(state.filtered_tickets.iter().all(|t| t.state != "closed"));
}

#[test]
fn rebuild_filtered_tickets_shows_closed_when_toggled() {
    let mut state = AppState::new();
    state.data.tickets = vec![
        make_ticket("1", "open"),
        make_ticket("2", "closed"),
        make_ticket("3", "open"),
    ];
    state.show_closed_tickets = true;
    state.rebuild_filtered_tickets();
    assert_eq!(state.filtered_tickets.len(), 3);
}

#[test]
fn rebuild_filtered_tickets_applies_text_filter() {
    let mut state = AppState::new();
    state.data.tickets = vec![
        make_ticket("1", "open"),
        make_ticket("2", "open"),
        make_ticket("3", "open"),
    ];
    state.show_closed_tickets = true;
    state.filter.active = true;
    state.filter.text = "Ticket 2".to_lowercase();
    state.rebuild_filtered_tickets();
    assert_eq!(state.filtered_tickets.len(), 1);
    assert_eq!(state.filtered_tickets[0].id, "2");
}

#[test]
fn rebuild_filtered_detail_tickets_independent_of_global() {
    let mut state = AppState::new();
    state.data.tickets = vec![make_ticket("1", "open"), make_ticket("2", "closed")];
    state.detail_tickets = vec![make_ticket("3", "open"), make_ticket("4", "closed")];
    state.show_closed_tickets = false;
    state.rebuild_filtered_tickets();
    assert_eq!(state.filtered_tickets.len(), 1);
    assert_eq!(state.filtered_detail_tickets.len(), 1);
    assert_eq!(state.filtered_tickets[0].id, "1");
    assert_eq!(state.filtered_detail_tickets[0].id, "3");
}

#[test]
fn filtered_tickets_index_matches_rendered_order() {
    let mut state = AppState::new();
    state.data.tickets = vec![
        make_ticket("1", "open"),
        make_ticket("2", "closed"),
        make_ticket("3", "open"),
        make_ticket("4", "open"),
    ];
    state.show_closed_tickets = false;
    state.rebuild_filtered_tickets();
    assert_eq!(state.filtered_tickets.len(), 3);
    assert_eq!(state.filtered_tickets[0].id, "1");
    assert_eq!(state.filtered_tickets[1].id, "3");
    assert_eq!(state.filtered_tickets[2].id, "4");
    assert_eq!(state.filtered_tickets[2].id, "4");
}

// --- status message auto-clear tests ---

#[test]
fn tick_status_message_clears_after_timeout() {
    let mut state = AppState::new();
    state.status_message = Some("hello".into());
    state.status_message_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(5));
    state.tick_status_message(std::time::Duration::from_secs(4));
    assert!(state.status_message.is_none());
    assert!(state.status_message_at.is_none());
}

#[test]
fn tick_status_message_keeps_message_within_timeout() {
    let mut state = AppState::new();
    state.status_message = Some("hello".into());
    state.status_message_at = Some(std::time::Instant::now());
    state.tick_status_message(std::time::Duration::from_secs(4));
    assert!(state.status_message.is_some());
    assert!(state.status_message_at.is_some());
}

#[test]
fn tick_status_message_no_op_when_none() {
    let mut state = AppState::new();
    state.tick_status_message(std::time::Duration::from_secs(4));
    assert!(state.status_message.is_none());
    assert!(state.status_message_at.is_none());
}

#[test]
fn track_status_message_change_sets_timestamp_on_appear() {
    let mut state = AppState::new();
    state.status_message = Some("new".into());
    state.track_status_message_change(false);
    assert!(state.status_message_at.is_some());
}

#[test]
fn track_status_message_change_clears_timestamp_on_disappear() {
    let mut state = AppState::new();
    state.status_message_at = Some(std::time::Instant::now());
    state.status_message = None;
    state.track_status_message_change(true);
    assert!(state.status_message_at.is_none());
}

#[test]
fn track_status_message_change_no_op_when_message_persists() {
    let mut state = AppState::new();
    let before = std::time::Instant::now() - std::time::Duration::from_secs(2);
    state.status_message = Some("persisting".into());
    state.status_message_at = Some(before);
    state.track_status_message_change(true);
    assert!(state.status_message_at.unwrap() <= before + std::time::Duration::from_millis(1));
}
