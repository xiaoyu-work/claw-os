use super::*;
use rayon::prelude::*;

use iced_test::{Error, simulator};

#[test]
#[ignore]
fn it_showcases_every_theme() -> Result<(), Error> {
    Theme::ALL
        .par_iter()
        .cloned()
        .map(|theme| {
            let mut styling = Styling::default();
            styling.update(Message::ThemeChanged(theme.clone()));

            let mut ui = simulator(styling.view());
            let snapshot = ui.snapshot(&theme)?;

            assert!(
                snapshot.matches_hash(format!(
                    "snapshots/{theme}",
                    theme = theme
                        .to_string()
                        .to_ascii_lowercase()
                        .replace(" ", "_")
                ))?,
                "snapshots for {theme} should match!"
            );

            Ok(())
        })
        .collect()
}
