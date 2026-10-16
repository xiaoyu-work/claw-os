use super::*;
use crate::activities::Response;
use serde_json::json;

const ID: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

fn ready() -> Activities {
    crate::localize::localize();
    Activities {visible:true,selected:Some(ID.into()),detail:Some(serde_json::from_value(json!({
        "activity":{"id":ID,"title":"Research","goal":"Prepare draft","state":"active"},"jobs":[]
    })).unwrap()),..Activities::default()}
}

fn update(state: &mut Activities, message: Message) -> Option<Request> {
    state.update(ActivityMessage::AutomaticWork(message), true)
}

#[test]
fn automatic_work_configuration_is_initially_stopped_and_conflicts_keep_the_draft() {
    let mut state = ready();
    let read = update(&mut state, Message::Refresh).unwrap();
    state.update(
        ActivityMessage::Loaded {
            generation: read.generation,
            result: Ok(Response::AutomaticWork(Box::new(
                ActivityAutoWorkResponse {
                    schema: 1,
                    activity_id: ID.into(),
                    work: None,
                    history: Vec::new(),
                },
            ))),
        },
        true,
    );
    update(&mut state, Message::Configure);
    update(
        &mut state,
        Message::Field(Field::Task, "Prepare draft".into()),
    );
    let write = update(&mut state, Message::Save).unwrap();
    assert!(matches!(&write.action,Action::SetAutomaticWork {request,..}
        if request.expected_revision.is_none() && request.work.check_interval_secs==3600));
    state.update(
        ActivityMessage::Loaded {
            generation: write.generation,
            result: Err("Conflict".into()),
        },
        true,
    );
    assert!(state.automatic_work.form.is_some());
    let retry = update(&mut state, Message::Save).unwrap();
    assert!(
        matches!(retry.action,Action::SetAutomaticWork {request,..} if request.expected_revision.is_none())
    );
    let _ = state.view(true);
}

#[test]
fn a_late_automatic_work_read_cannot_replace_another_activity() {
    let mut state = ready();
    let read = update(&mut state, Message::Refresh).unwrap();
    state.update(
        ActivityMessage::Open("bbbbbbbb-bbbb-4ccc-8ddd-eeeeeeeeeeee".into()),
        true,
    );
    state.update(
        ActivityMessage::Loaded {
            generation: read.generation,
            result: Ok(Response::AutomaticWork(Box::new(
                ActivityAutoWorkResponse {
                    schema: 1,
                    activity_id: ID.into(),
                    work: None,
                    history: Vec::new(),
                },
            ))),
        },
        true,
    );
    assert!(state.automatic_work.response.is_none());
    assert_eq!(
        state.selected.as_deref(),
        Some("bbbbbbbb-bbbb-4ccc-8ddd-eeeeeeeeeeee")
    );
}
