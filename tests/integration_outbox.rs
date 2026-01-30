//! Outbox: insert_pending, list_pending (only Pending), mark_published; after mark, list excludes it.

use bpm_engine::persistence::{MemoryRepo, OutboxRepo};

#[test]
fn outbox_insert_list_mark() {
    let repo = std::sync::Arc::new(MemoryRepo::new());
    let id1 = repo.insert_pending("ev1", "{}").unwrap();
    let id2 = repo.insert_pending("ev2", "{}").unwrap();
    let id3 = repo.insert_pending("ev3", "{}").unwrap();

    let pending = repo.list_pending().unwrap();
    assert_eq!(pending.len(), 3);

    repo.mark_published(&id1).unwrap();
    let pending = repo.list_pending().unwrap();
    assert_eq!(pending.len(), 2);
    assert!(pending.iter().all(|e| e.id != id1));

    repo.mark_published(&id2).unwrap();
    repo.mark_published(&id3).unwrap();
    let pending = repo.list_pending().unwrap();
    assert!(pending.is_empty());
}
