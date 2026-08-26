use super::*;
use iced_test::{Error, simulator};

#[test]
fn it_counts() -> Result<(), Error> {
    let mut counter = Counter { value: 0 };
    let mut ui = simulator(counter.view());

    let _ = ui.click("Increment")?;
    let _ = ui.click("Increment")?;
    let _ = ui.click("Decrement")?;

    for message in ui.into_messages() {
        counter.update(message);
    }

    assert_eq!(counter.value, 1);

    let mut ui = simulator(counter.view());
    assert!(ui.find("1").is_ok(), "Counter should display 1!");

    Ok(())
}
