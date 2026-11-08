use super::*;

fn preview_job(meta: &crate::session::SessionMeta) -> Job {
    Job::new_pending_with_client(
        "Synthetic stopped preview".into(),
        None,
        None,
        Some(meta.id.to_string()),
        Some(1),
        Some(1000),
        Some("/home/example".into()),
        Default::default(),
    )
}

fn preview_link(target: ReadTarget) -> workspaces::ReadLink {
    workspaces::ReadLink {
        id: uuid::Uuid::new_v4().to_string(),
        source_workspace_id: uuid::Uuid::new_v4().to_string(),
        target,
        note_ids: vec![uuid::Uuid::new_v4().to_string()],
        sources: Vec::new(),
        expires_at: (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
        created_at: chrono::Utc::now().to_rfc3339(),
    }
}

#[test]
fn human_job_preview_cannot_widen_to_its_conversation_or_another_job() {
    let meta = crate::session::SessionMeta::fresh(crate::session::SessionId::generate(), "Preview");
    let job = preview_job(&meta);
    let link = preview_link(ReadTarget::Job { id: job.id.clone() });
    assert!(link_matches_for(&link, Some(&job), None));
    assert!(!link_matches_for(&link, None, Some(&meta)));
    assert!(!link_matches_for(&link, Some(&preview_job(&meta)), None));
    assert!(!link_matches_for(&link, None, None));
}

#[test]
fn human_conversation_preview_is_exact_and_expiry_still_closes_it() {
    let meta = crate::session::SessionMeta::fresh(crate::session::SessionId::generate(), "Preview");
    let other = crate::session::SessionMeta::fresh(crate::session::SessionId::generate(), "Other");
    let mut link = preview_link(ReadTarget::Conversation {
        id: meta.id.to_string(),
    });
    assert!(link_matches_for(&link, None, Some(&meta)));
    assert!(link_matches_for(&link, Some(&preview_job(&meta)), None));
    assert!(!link_matches_for(&link, None, Some(&other)));
    assert!(!link_matches_for(&link, Some(&preview_job(&other)), None));
    link.expires_at = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
    assert!(!link_matches_for(&link, None, Some(&meta)));
}
