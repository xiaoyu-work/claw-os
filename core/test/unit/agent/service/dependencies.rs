use super::*;

#[test]
fn cancelled_intermediate_followup_does_not_bypass_an_unfinished_ancestor() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::with_root(root.path().join("jobs")).unwrap();
    let mut primary = Job::new_pending("primary".into(), None, None, None, None, None, None);
    primary.status = JobStatus::WaitingApproval;
    let mut cancelled = Job::new_pending("cancelled".into(), None, None, None, None, None, None);
    cancelled.status = JobStatus::Cancelled;
    cancelled.after_task_id = Some(primary.id.clone());
    let mut next = Job::new_pending("next".into(), None, None, None, None, None, None);
    next.after_task_id = Some(cancelled.id.clone());
    std::fs::write(
        store
            .root()
            .join("waiting")
            .join(format!("{}.json", primary.id)),
        serde_json::to_vec(&primary).unwrap(),
    )
    .unwrap();
    std::fs::write(
        store
            .root()
            .join("done")
            .join(format!("{}.json", cancelled.id)),
        serde_json::to_vec(&cancelled).unwrap(),
    )
    .unwrap();
    assert!(matches!(state(&store, &next).unwrap(), Dependency::Blocked));
    primary.status = JobStatus::Ok;
    std::fs::remove_file(
        store
            .root()
            .join("waiting")
            .join(format!("{}.json", primary.id)),
    )
    .unwrap();
    std::fs::write(
        store
            .root()
            .join("done")
            .join(format!("{}.json", primary.id)),
        serde_json::to_vec(&primary).unwrap(),
    )
    .unwrap();
    assert!(matches!(state(&store, &next).unwrap(), Dependency::Ready));
}
