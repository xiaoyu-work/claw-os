//! `com.clawos.Theme.{Light,Dark}` is a *derived* config: every field is
//! whatever `ThemeBuilder::build()` produced from the matching
//! `.Builder` entry. The two drifted apart once already — the builders
//! carried the Claw palette while the derived themes still held stock
//! COSMIC greys — and nothing caught it, because cosmic-config falls
//! back to compiled-in defaults on a parse miss rather than failing
//! loudly. Re-derive here and compare.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use cosmic_theme::ThemeBuilder;

fn schema_dir(entry: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../settings/resources/default_schema")
        .join(entry)
        .join("v1")
}

fn read_fields(dir: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for entry in fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .flatten()
    {
        if entry.file_type().is_ok_and(|t| t.is_file()) {
            out.insert(
                entry.file_name().to_string_lossy().into_owned(),
                fs::read_to_string(entry.path()).expect("read field"),
            );
        }
    }
    out
}

#[track_caller]
fn parse<T: serde::de::DeserializeOwned>(fields: &HashMap<String, String>, key: &str) -> T {
    let raw = fields
        .get(key)
        .unwrap_or_else(|| panic!("missing field {key}"));
    ron::from_str(raw.trim()).unwrap_or_else(|e| panic!("parse {key}: {e}"))
}

fn builder_from(fields: &HashMap<String, String>, is_dark: bool) -> ThemeBuilder {
    let mut b = if is_dark {
        ThemeBuilder::dark()
    } else {
        ThemeBuilder::light()
    };
    b.palette = parse(fields, "palette");
    b.spacing = parse(fields, "spacing");
    b.corner_radii = parse(fields, "corner_radii");
    b.neutral_tint = parse(fields, "neutral_tint");
    b.bg_color = parse(fields, "bg_color");
    b.primary_container_bg = parse(fields, "primary_container_bg");
    b.secondary_container_bg = parse(fields, "secondary_container_bg");
    b.text_tint = parse(fields, "text_tint");
    b.accent = parse(fields, "accent");
    b.success = parse(fields, "success");
    b.warning = parse(fields, "warning");
    b.destructive = parse(fields, "destructive");
    b.is_frosted = parse(fields, "is_frosted");
    b.gaps = parse(fields, "gaps");
    b.active_hint = parse(fields, "active_hint");
    b.window_hint = parse(fields, "window_hint");
    b
}

/// Rebuild a shipped theme from its builder and assert the on-disk
/// derived fields still match.
fn assert_derived(mode: &str, is_dark: bool) {
    let builder_fields = read_fields(&schema_dir(&format!("com.clawos.Theme.{mode}.Builder")));
    let theme_fields = read_fields(&schema_dir(&format!("com.clawos.Theme.{mode}")));

    let expected = builder_from(&builder_fields, is_dark).build();

    // `build()` labels the theme after its seed palette; the shipped
    // schema keeps the Claw name, so that one field differs by design.
    let name: String = parse(&theme_fields, "name");
    assert_eq!(name, format!("claw-{}", mode.to_lowercase()));

    assert_eq!(parse::<bool>(&theme_fields, "is_dark"), is_dark);

    macro_rules! same {
        ($($field:ident),* $(,)?) => {
            $({
                // Compare the serialized form rather than the parsed
                // value: it sidesteps per-field type annotations and
                // additionally pins the on-disk layout to exactly what
                // `cosmic_config::Config::set` writes at runtime.
                let want = ron::ser::to_string_pretty(
                    &expected.$field,
                    ron::ser::PrettyConfig::new(),
                )
                .expect("serialize");
                let got = theme_fields
                    .get(stringify!($field))
                    .unwrap_or_else(|| panic!("missing field {}", stringify!($field)));
                assert_eq!(
                    got.trim_end(),
                    want.trim_end(),
                    concat!(
                        stringify!($field),
                        " is stale — re-run `cargo run --example regen_default_theme`",
                    ),
                );
            })*
        };
    }

    same!(
        background,
        primary,
        secondary,
        accent,
        success,
        destructive,
        warning,
        accent_button,
        success_button,
        destructive_button,
        warning_button,
        icon_button,
        link_button,
        list_button,
        text_button,
        button,
        palette,
        spacing,
        corner_radii,
        is_high_contrast,
        gaps,
        active_hint,
        window_hint,
        is_frosted,
        shade,
        accent_text,
        control_tint,
        text_tint,
    );
}

#[test]
fn light_theme_is_derived_from_its_builder() {
    assert_derived("Light", false);
}

#[test]
fn dark_theme_is_derived_from_its_builder() {
    assert_derived("Dark", true);
}

/// The desktop ships light-first, and the brand accent is Claw blue
/// (`#005CFE`) in both modes.
#[test]
fn shipped_defaults_are_light_and_branded() {
    let mode_dir = schema_dir("com.clawos.Theme.Mode");
    let is_dark: bool =
        ron::from_str(fs::read_to_string(mode_dir.join("is_dark")).unwrap().trim()).unwrap();
    assert!(!is_dark, "fresh installs boot into light mode");

    for (mode, dark) in [("Light", false), ("Dark", true)] {
        // `assert_derived` already pins the on-disk theme to this
        // builder, so reading the accent off the rebuilt theme is
        // equivalent to reading it off disk.
        let theme = builder_from(
            &read_fields(&schema_dir(&format!("com.clawos.Theme.{mode}.Builder"))),
            dark,
        )
        .build();

        let accent = theme.accent_color();
        let hex = (
            (accent.red * 255.0).round() as u8,
            (accent.green * 255.0).round() as u8,
            (accent.blue * 255.0).round() as u8,
        );
        assert_eq!(hex, (0x00, 0x5C, 0xFE), "{mode} accent is not Claw blue");

        let fields = read_fields(&schema_dir(&format!("com.clawos.Theme.{mode}")));
        assert_eq!(parse::<bool>(&fields, "is_dark"), dark);
    }
}
