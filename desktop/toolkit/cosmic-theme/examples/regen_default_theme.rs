//! One-shot regenerator for the shipped default theme schemas.
//!
//! `com.clawos.Theme.{Light,Dark}` are *derived* configs: every field
//! is produced by running `ThemeBuilder::build()` over the matching
//! `com.clawos.Theme.{Light,Dark}.Builder` values. The two had drifted
//! apart — the builders carried the Claw brand palette while the
//! derived themes still held stock COSMIC greys, so booting into light
//! mode rendered a grey desktop instead of the cool blue-white one the
//! brand calls for.
//!
//! Run with the default_schema directory as the sole argument:
//!
//! ```ignore
//! cargo run --example regen_default_theme -- ../../settings/resources/default_schema
//! ```
//!
//! Reads `<dir>/com.clawos.Theme.<Mode>.Builder/v1/*`, rebuilds the
//! theme, and rewrites `<dir>/com.clawos.Theme.<Mode>/v1/*` using the
//! same per-field RON layout `cosmic-config` writes at runtime.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use cosmic_theme::ThemeBuilder;

/// Serialize with the exact settings `cosmic_config::Config` uses so
/// the regenerated files stay byte-comparable with runtime writes.
fn to_ron<T: serde::Serialize>(value: &T) -> String {
    ron::ser::to_string_pretty(value, ron::ser::PrettyConfig::new()).expect("serialize")
}

/// Read every `v1/<field>` file of a config directory into a map of
/// field name -> raw RON text.
fn read_fields(dir: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let text = fs::read_to_string(entry.path()).expect("read field");
        out.insert(name, text);
    }
    out
}

/// Rebuild a `ThemeBuilder` from its on-disk fields. Anything absent
/// falls back to the builder default for that mode, matching how
/// `CosmicConfigEntry::get_entry` degrades on a partial config.
fn builder_from_fields(fields: &HashMap<String, String>, is_dark: bool) -> ThemeBuilder {
    let mut builder = if is_dark {
        ThemeBuilder::dark()
    } else {
        ThemeBuilder::light()
    };

    macro_rules! load {
        ($key:literal, $field:ident) => {
            if let Some(raw) = fields.get($key) {
                match ron::from_str(raw) {
                    Ok(v) => builder.$field = v,
                    Err(e) => panic!("parse {}: {e}", $key),
                }
            }
        };
    }

    load!("palette", palette);
    load!("spacing", spacing);
    load!("corner_radii", corner_radii);
    load!("neutral_tint", neutral_tint);
    load!("bg_color", bg_color);
    load!("primary_container_bg", primary_container_bg);
    load!("secondary_container_bg", secondary_container_bg);
    load!("text_tint", text_tint);
    load!("accent", accent);
    load!("success", success);
    load!("warning", warning);
    load!("destructive", destructive);
    load!("is_frosted", is_frosted);
    load!("gaps", gaps);
    load!("active_hint", active_hint);
    load!("window_hint", window_hint);

    builder
}

fn regen(schema_dir: &Path, mode: &str, is_dark: bool) {
    let builder_dir = schema_dir.join(format!("com.clawos.Theme.{mode}.Builder/v1"));
    let theme_dir = schema_dir.join(format!("com.clawos.Theme.{mode}/v1"));

    let builder = builder_from_fields(&read_fields(&builder_dir), is_dark);
    let mut theme = builder.build();

    // `build()` names the theme after the seed palette ("cosmic-light").
    // The shipped schema is the Claw-branded one, so keep whatever name
    // is already on disk rather than reverting to the upstream label.
    if let Ok(existing) = fs::read_to_string(theme_dir.join("name"))
        && let Ok(name) = ron::from_str::<String>(existing.trim())
    {
        theme.name = name;
    }

    // Each `Theme` field lands in its own file, serialized exactly the
    // way `cosmic_config::Config::set` would write it. Listing the
    // fields explicitly (rather than reflecting over a `ron::Value`)
    // keeps RON's struct syntax — a `ron::Value` round-trip degrades
    // structs into string-keyed maps that no longer match the format
    // every other shipped schema file uses.
    let mut written = 0usize;
    macro_rules! emit {
        ($($field:ident),* $(,)?) => {
            $(
                fs::write(
                    theme_dir.join(stringify!($field)),
                    to_ron(&theme.$field),
                )
                .expect("write field");
                written += 1;
            )*
        };
    }

    emit!(
        name,
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
        is_dark,
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

    println!("{mode}: wrote {written} fields to {}", theme_dir.display());
}

fn main() {
    let schema_dir = std::env::args()
        .nth(1)
        .expect("usage: regen_default_theme <default_schema dir>");
    let schema_dir = Path::new(&schema_dir);

    regen(schema_dir, "Light", false);
    regen(schema_dir, "Dark", true);
}
