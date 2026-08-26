use super::*;
use crate::SearchResultCategory;

#[test]
fn category_defaults_to_unknown() {
    let config: PluginConfig = ron::from_str(
        r#"(
            name: "Third party",
            description: "No category field",
            bin: (path: "plugin"),
        )"#,
    )
    .unwrap();

    assert_eq!(config.category, SearchResultCategory::Unknown);
}

#[test]
fn shipped_plugin_categories_parse() {
    let configs = [
        (
            include_str!("../../../plugins/src/desktop_entries/plugin.ron"),
            SearchResultCategory::Apps,
        ),
        (
            include_str!("../../../plugins/src/files/plugin.ron"),
            SearchResultCategory::Files,
        ),
        (
            include_str!("../../../plugins/src/recent/plugin.ron"),
            SearchResultCategory::Recent,
        ),
        (
            include_str!("../../../plugins/src/terminal/plugin.ron"),
            SearchResultCategory::Commands,
        ),
        (
            include_str!("../../../plugins/src/pulse/plugin.ron"),
            SearchResultCategory::Settings,
        ),
    ];

    for (ron, expected) in configs {
        let config: PluginConfig = ron::from_str(ron).unwrap();
        assert_eq!(config.category, expected);
    }
}
