use super::*;
use crate::activities::{ActivityService, DATABASE_SCHEMA_VERSION};

fn draft() -> crate::activities::ActivityDraft {
    crate::activities::ActivityDraft {
        title: "Release".into(),
        goal: "Prepare the release".into(),
        completion_criteria: String::new(),
        boundaries: String::new(),
        resources: Vec::new(),
    }
}

#[test]
fn policy_is_owner_scoped_optional_and_revision_checked() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = service.create(1000, draft()).unwrap();
    assert!(service
        .scheduling_policy(1000, &activity.id)
        .unwrap()
        .is_none());
    for owner in [0, 2000] {
        assert!(matches!(
            service.scheduling_policy(owner, &activity.id),
            Err(ActivityError::NotFound)
        ));
        assert!(matches!(
            service.set_scheduling_policy(
                owner,
                &activity.id,
                None,
                ActivitySchedulingPriority::Foreground,
            ),
            Err(ActivityError::NotFound)
        ));
    }

    let created = service
        .set_scheduling_policy(
            1000,
            &activity.id,
            None,
            ActivitySchedulingPriority::Foreground,
        )
        .unwrap();
    assert_eq!(created.revision, 1);
    assert_eq!(created.owner_uid, 1000);
    assert_eq!(created.priority, ActivitySchedulingPriority::Foreground);
    assert!(service
        .set_scheduling_policy(
            1000,
            &activity.id,
            None,
            ActivitySchedulingPriority::Background,
        )
        .is_err());
    assert!(service
        .set_scheduling_policy(
            1000,
            &activity.id,
            Some(2),
            ActivitySchedulingPriority::Background,
        )
        .is_err());
    let updated = service
        .set_scheduling_policy(
            1000,
            &activity.id,
            Some(1),
            ActivitySchedulingPriority::Background,
        )
        .unwrap();
    assert_eq!(updated.revision, 2);
    assert_eq!(updated.priority, ActivitySchedulingPriority::Background);
    assert_eq!(
        service
            .scheduling_policy(1000, &activity.id)
            .unwrap()
            .unwrap(),
        updated
    );
}

#[test]
fn paused_policy_is_configurable_but_terminal_policy_requires_reopen() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = service.create(7, draft()).unwrap();
    service
        .transition(7, &activity.id, ActivityState::Paused, None)
        .unwrap();
    let paused = service
        .set_scheduling_policy(
            7,
            &activity.id,
            None,
            ActivitySchedulingPriority::Background,
        )
        .unwrap();
    service
        .transition(7, &activity.id, ActivityState::Completed, Some("Done".into()))
        .unwrap();
    assert!(service
        .set_scheduling_policy(
            7,
            &activity.id,
            Some(paused.revision),
            ActivitySchedulingPriority::Foreground,
        )
        .is_err());
    assert_eq!(
        service
            .scheduling_policy(7, &activity.id)
            .unwrap()
            .unwrap(),
        paused
    );
    service
        .transition(7, &activity.id, ActivityState::Active, None)
        .unwrap();
    assert_eq!(
        service
            .set_scheduling_policy(
                7,
                &activity.id,
                Some(paused.revision),
                ActivitySchedulingPriority::Standard,
            )
            .unwrap()
            .revision,
        paused.revision + 1
    );
}

#[test]
fn schema_six_migrates_to_seven_without_changing_activity_rows() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("activities.db");
    let activity = {
        let service = SqliteActivityService::open(&path).unwrap();
        service.create(31, draft()).unwrap()
    };
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TABLE activity_scheduling_policies;
             PRAGMA user_version = 6;",
        )
        .unwrap();
    }
    let service = SqliteActivityService::open(&path).unwrap();
    assert_eq!(service.get(31, &activity.id).unwrap(), activity);
    assert!(service
        .scheduling_policy(31, &activity.id)
        .unwrap()
        .is_none());
    assert_eq!(
        service
            .lock()
            .unwrap()
            .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
            .unwrap(),
        DATABASE_SCHEMA_VERSION
    );
}

#[test]
fn corrupt_policy_values_and_missing_schema_fail_explicitly() {
    let service = SqliteActivityService::open_in_memory().unwrap();
    let activity = service.create(7, draft()).unwrap();
    service
        .set_scheduling_policy(
            7,
            &activity.id,
            None,
            ActivitySchedulingPriority::Standard,
        )
        .unwrap();
    service
        .lock()
        .unwrap()
        .execute_batch("PRAGMA ignore_check_constraints = ON;")
        .unwrap();
    service
        .lock()
        .unwrap()
        .execute(
            "UPDATE activity_scheduling_policies SET priority = 'urgent'
             WHERE owner_uid = 7 AND activity_id = ?1",
            [activity.id.as_str()],
        )
        .unwrap();
    assert!(matches!(
        service.scheduling_policy(7, &activity.id),
        Err(ActivityError::Corrupt(_))
    ));

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("activities.db");
    {
        let service = SqliteActivityService::open(&path).unwrap();
        drop(service);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TABLE activity_scheduling_policies;
             PRAGMA user_version = 7;",
        )
        .unwrap();
    }
    assert!(matches!(
        SqliteActivityService::open(&path),
        Err(ActivityError::Database(_))
    ));
}
