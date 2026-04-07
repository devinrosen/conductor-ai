use super::*;

#[test]
fn agent_activity_len_empty() {
    let cache = DataCache::default();
    assert_eq!(cache.agent_activity_len(), 0);
    assert_eq!(cache.agent_activity_len(), cache.visual_rows().len());
}

#[test]
fn agent_activity_len_single_run() {
    let mut cache = DataCache {
        agent_events: vec![make_event("e1", "r1"), make_event("e2", "r1")],
        ..Default::default()
    };
    cache
        .agent_run_info
        .insert("r1".into(), (1, None, "2026-01-01T00:00:00Z".into()));
    assert_eq!(cache.agent_activity_len(), 2);
    assert_eq!(cache.agent_activity_len(), cache.visual_rows().len());
}

#[test]
fn agent_activity_len_multiple_runs() {
    let mut cache = DataCache {
        agent_events: vec![
            make_event("e1", "r1"),
            make_event("e2", "r1"),
            make_event("e3", "r2"),
        ],
        ..Default::default()
    };
    cache
        .agent_run_info
        .insert("r1".into(), (1, None, "2026-01-01T00:00:00Z".into()));
    cache
        .agent_run_info
        .insert("r2".into(), (2, None, "2026-01-01T00:01:00Z".into()));
    assert_eq!(cache.agent_activity_len(), 5);
    assert_eq!(cache.agent_activity_len(), cache.visual_rows().len());
}

#[test]
fn agent_activity_len_interleaved_runs() {
    let mut cache = DataCache {
        agent_events: vec![
            make_event("e1", "r1"),
            make_event("e2", "r2"),
            make_event("e3", "r1"),
        ],
        ..Default::default()
    };
    cache
        .agent_run_info
        .insert("r1".into(), (1, None, "2026-01-01T00:00:00Z".into()));
    cache
        .agent_run_info
        .insert("r2".into(), (2, None, "2026-01-01T00:01:00Z".into()));
    assert_eq!(cache.agent_activity_len(), 6);
    assert_eq!(cache.agent_activity_len(), cache.visual_rows().len());
}

// --- Repo agent visual-row helpers ---

#[test]
fn repo_agent_activity_len_empty() {
    let cache = DataCache::default();
    assert_eq!(cache.repo_agent_activity_len(), 0);
    assert_eq!(
        cache.repo_agent_activity_len(),
        cache.repo_agent_visual_rows().len()
    );
}

#[test]
fn repo_agent_activity_len_single_run() {
    let mut cache = DataCache {
        repo_agent_events: vec![make_event("e1", "r1"), make_event("e2", "r1")],
        ..Default::default()
    };
    cache
        .repo_agent_run_info
        .insert("r1".into(), (1, None, "2026-01-01T00:00:00Z".into()));
    assert_eq!(cache.repo_agent_activity_len(), 2);
    assert_eq!(
        cache.repo_agent_activity_len(),
        cache.repo_agent_visual_rows().len()
    );
}

#[test]
fn repo_agent_activity_len_multiple_runs() {
    let mut cache = DataCache {
        repo_agent_events: vec![
            make_event("e1", "r1"),
            make_event("e2", "r1"),
            make_event("e3", "r2"),
        ],
        ..Default::default()
    };
    cache
        .repo_agent_run_info
        .insert("r1".into(), (1, None, "2026-01-01T00:00:00Z".into()));
    cache
        .repo_agent_run_info
        .insert("r2".into(), (2, None, "2026-01-01T00:01:00Z".into()));
    assert_eq!(cache.repo_agent_activity_len(), 5);
    assert_eq!(
        cache.repo_agent_activity_len(),
        cache.repo_agent_visual_rows().len()
    );
}

#[test]
fn repo_agent_event_at_visual_index_returns_event() {
    let mut cache = DataCache {
        repo_agent_events: vec![make_event("e1", "r1"), make_event("e2", "r1")],
        ..Default::default()
    };
    cache
        .repo_agent_run_info
        .insert("r1".into(), (1, None, "2026-01-01T00:00:00Z".into()));
    assert_eq!(cache.repo_agent_event_at_visual_index(0).unwrap().id, "e1");
    assert_eq!(cache.repo_agent_event_at_visual_index(1).unwrap().id, "e2");
    assert!(cache.repo_agent_event_at_visual_index(2).is_none());
}

#[test]
fn repo_agent_event_at_visual_index_skips_separator() {
    let mut cache = DataCache {
        repo_agent_events: vec![make_event("e1", "r1"), make_event("e2", "r2")],
        ..Default::default()
    };
    cache
        .repo_agent_run_info
        .insert("r1".into(), (1, None, "2026-01-01T00:00:00Z".into()));
    cache
        .repo_agent_run_info
        .insert("r2".into(), (2, None, "2026-01-01T00:01:00Z".into()));
    assert!(cache.repo_agent_event_at_visual_index(0).is_none());
    assert_eq!(cache.repo_agent_event_at_visual_index(1).unwrap().id, "e1");
    assert!(cache.repo_agent_event_at_visual_index(2).is_none());
    assert_eq!(cache.repo_agent_event_at_visual_index(3).unwrap().id, "e2");
}
