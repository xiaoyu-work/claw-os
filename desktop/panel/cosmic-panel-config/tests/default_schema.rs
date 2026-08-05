//! The shipped `data/default_schema` files are what a fresh install
//! boots with. cosmic-config silently falls back to the compiled-in
//! `Default` when a field fails to parse, so a typo there is invisible
//! until someone notices the panel looks wrong. Parse every field with
//! its real type instead.

use std::path::{Path, PathBuf};

use cosmic_panel_config::{CosmicPanelBackground, PanelAnchor, PanelSize};

fn schema_dir(entry: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../data/default_schema")
        .join(entry)
        .join("v1")
}

#[track_caller]
fn parse<T: serde::de::DeserializeOwned>(entry: &str, field: &str) -> T {
    let path = schema_dir(entry).join(field);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    ron::from_str(raw.trim()).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// Every field of both shipped panel entries must round-trip through
/// the type the panel actually reads it as.
#[test]
fn shipped_schema_parses() {
    assert_eq!(
        parse::<Vec<String>>("com.clawos.Panel", "entries"),
        vec!["Panel".to_string(), "Dock".to_string()],
    );

    for entry in ["com.clawos.Panel.Panel", "com.clawos.Panel.Dock"] {
        let _: PanelAnchor = parse(entry, "anchor");
        let _: PanelSize = parse(entry, "size");
        let _: CosmicPanelBackground = parse(entry, "background");
        let _: String = parse(entry, "name");
        let _: bool = parse(entry, "anchor_gap");
        let _: bool = parse(entry, "expand_to_edges");
        let _: bool = parse(entry, "exclusive_zone");
        let _: u32 = parse(entry, "padding");
        let _: u32 = parse(entry, "spacing");
        let _: u32 = parse(entry, "border_radius");
        let _: u32 = parse(entry, "margin");
        let _: f32 = parse(entry, "opacity");
        let _: f32 = parse(entry, "padding_overlap");
        let _: Option<u32> = parse(entry, "autohover_delay_ms");
        let _: Option<(Vec<String>, Vec<String>)> = parse(entry, "plugins_wings");
        let _: Option<Vec<String>> = parse(entry, "plugins_center");
    }
}

/// The top bar carries the shell's identity: branding and workspaces on
/// the left, the agent status in the middle, indicators trailing into
/// the clock on the right.
#[test]
fn top_panel_layout() {
    let entry = "com.clawos.Panel.Panel";

    assert_eq!(parse::<PanelAnchor>(entry, "anchor"), PanelAnchor::Top);

    let (left, right) = parse::<Option<(Vec<String>, Vec<String>)>>(entry, "plugins_wings")
        .expect("top panel has wings");

    assert_eq!(
        left,
        ["com.clawos.PanelAppButton", "com.clawos.AppletWorkspaces"],
    );
    assert_eq!(
        right.last().map(String::as_str),
        Some("com.clawos.AppletTime"),
        "the clock stays right-most",
    );

    assert_eq!(
        parse::<Option<Vec<String>>>(entry, "plugins_center"),
        Some(vec!["com.clawos.AppletAgentActivity".to_string()]),
    );
}

/// The dock is a vertical bar on the left edge holding pinned apps and
/// the app library, with no duplicate workspace or minimize controls.
#[test]
fn dock_layout() {
    let entry = "com.clawos.Panel.Dock";

    assert_eq!(parse::<PanelAnchor>(entry, "anchor"), PanelAnchor::Left);
    assert_eq!(parse::<Option<(Vec<String>, Vec<String>)>>(entry, "plugins_wings"), None);

    let center = parse::<Option<Vec<String>>>(entry, "plugins_center").expect("dock has plugins");
    assert_eq!(center, ["com.clawos.AppList", "com.clawos.PanelAppButton"]);

    // A capsule radius on a vertical bar rounds the whole column away;
    // the dock wants a rounded rectangle.
    let radius = parse::<u32>(entry, "border_radius");
    assert!((16..=32).contains(&radius), "unexpected dock radius {radius}");
}
