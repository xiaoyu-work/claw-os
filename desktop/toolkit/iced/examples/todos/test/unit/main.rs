use super::*;

use iced::{Settings, Theme};
use iced_test::selector::id;
use iced_test::{Error, Simulator};

fn simulator(todos: &Todos) -> Simulator<'_, Message> {
    Simulator::with_settings(
        Settings {
            fonts: vec![Todos::ICON_FONT.into()],
            ..Settings::default()
        },
        todos.view(),
    )
}

#[test]
#[ignore]
fn it_creates_a_new_task() -> Result<(), Error> {
    let (mut todos, _command) = Todos::new();
    let _command = todos.update(Message::Loaded(Err(LoadError::File)));

    let mut ui = simulator(&todos);
    let _input = ui.click(id("new-task"))?;

    let _ = ui.typewrite("Create the universe");
    let _ = ui.tap_key(keyboard::key::Named::Enter);

    for message in ui.into_messages() {
        let _command = todos.update(message);
    }

    let mut ui = simulator(&todos);
    let _ = ui.find("Create the universe")?;

    let snapshot = ui.snapshot(&Theme::Dark)?;
    assert!(
        snapshot.matches_hash("snapshots/creates_a_new_task")?,
        "snapshots should match!"
    );

    Ok(())
}

#[test]
#[ignore]
fn it_passes_the_ice_tests() -> Result<(), Error> {
    iced_test::run(
        application(),
        format!("{}/tests", env!("CARGO_MANIFEST_DIR")),
    )
}
