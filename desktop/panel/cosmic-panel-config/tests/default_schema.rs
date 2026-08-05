//! The shipped `data/default_schema` files are what a fresh install
//! boots with. cosmic-config silently falls back to the compiled-in
//! `Default` when a field fails to parse, so a typo there is invisible
//! until someone notices the panel looks wrong. Parse every field with
//! its real type instead.

use std::path::{Path, PathBuf};

use cosmic_panel_config::{AutoHide, CosmicPanelConfig, CosmicPanelOuput, PanelAnchor, PanelSize};
use xdg_shell_wrapper_config::{KeyboardInteractivity, Layer};

fn schema_dir(entry: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/default_schema").join(entry).join("v1")
}

#[track_caller]
fn parse<T: serde::de::DeserializeOwned>(entry: &str, field: &str) -> T {
    let path = schema_dir(entry).join(field);
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    ron::from_str(raw.trim()).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn shipped_panel(entry: &str) -> CosmicPanelConfig {
    CosmicPanelConfig {
        name: parse(entry, "name"),
        anchor: parse(entry, "anchor"),
        anchor_gap: parse(entry, "anchor_gap"),
        layer: parse::<Layer>(entry, "layer"),
        keyboard_interactivity: parse::<KeyboardInteractivity>(entry, "keyboard_interactivity"),
        size: parse(entry, "size"),
        output: parse::<CosmicPanelOuput>(entry, "output"),
        background: parse(entry, "background"),
        plugins_wings: parse(entry, "plugins_wings"),
        plugins_center: parse(entry, "plugins_center"),
        size_wings: parse::<Option<(Option<PanelSize>, Option<PanelSize>)>>(entry, "size_wings"),
        size_center: parse::<Option<PanelSize>>(entry, "size_center"),
        expand_to_edges: parse(entry, "expand_to_edges"),
        padding: parse(entry, "padding"),
        spacing: parse(entry, "spacing"),
        border_radius: parse(entry, "border_radius"),
        exclusive_zone: parse(entry, "exclusive_zone"),
        autohide: parse::<Option<AutoHide>>(entry, "autohide"),
        margin: parse(entry, "margin"),
        opacity: parse(entry, "opacity"),
        autohover_delay_ms: parse(entry, "autohover_delay_ms"),
        padding_overlap: parse(entry, "padding_overlap"),
    }
}

fn as_value<T: serde::Serialize>(value: &T) -> ron::Value {
    let raw = ron::ser::to_string(value).expect("serialize config");
    ron::from_str(&raw).expect("deserialize config value")
}

/// Every field of both shipped panel entries must round-trip through
/// the type the panel actually reads it as.
#[test]
fn shipped_schema_parses() {
    assert_eq!(
        parse::<Vec<String>>("com.clawos.Panel", "entries"),
        vec!["Panel".to_string(), "Dock".to_string(), "WidgetRail".to_string(),],
    );

    for entry in ["com.clawos.Panel.Panel", "com.clawos.Panel.Dock", "com.clawos.Panel.WidgetRail"]
    {
        let _ = shipped_panel(entry);
    }
}

/// The top bar carries the shell's identity: branding and workspaces on
/// the left, the agent status in the middle, indicators trailing into
/// the clock on the right.
#[test]
fn top_panel_layout() {
    let entry = "com.clawos.Panel.Panel";

    assert_eq!(parse::<PanelAnchor>(entry, "anchor"), PanelAnchor::Top);
    assert_eq!(parse::<PanelSize>(entry, "size"), PanelSize::XS);
    assert_eq!(parse::<u16>(entry, "margin"), 0, "the top bar sits flush to the screen edge");
    assert_eq!(parse::<u32>(entry, "padding"), 4);
    assert_eq!(PanelSize::XS.get_applet_icon_size(true), 16);
    assert_eq!(PanelSize::XS.get_applet_padding(true), 8);
    assert_eq!(16 + 2 * 8 + 2 * 4, 40, "XS symbolic controls plus panel padding define a 40px bar",);

    let (left, right) = parse::<Option<(Vec<String>, Vec<String>)>>(entry, "plugins_wings")
        .expect("top panel has wings");

    assert_eq!(left, ["com.clawos.PanelBrandButton", "com.clawos.AppletWorkspaces"],);
    assert_eq!(
        right,
        [
            "com.clawos.PanelLauncherButton",
            "com.clawos.PanelCalendarButton",
            "com.clawos.AppletApprovalGate",
            "com.clawos.AppletNetwork",
            "com.clawos.AppletAudio",
            "com.clawos.AppletBattery",
            "com.clawos.AppletTime",
        ],
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
    assert_eq!(parse::<PanelSize>(entry, "size"), PanelSize::M);
    assert_eq!(parse::<u16>(entry, "margin"), 24);
    assert_eq!(parse::<u32>(entry, "padding"), 2);
    assert_eq!(PanelSize::M.get_applet_icon_size(false), 40);
    assert_eq!(PanelSize::M.get_applet_icon_size(true), 28);
    assert_eq!(PanelSize::M.get_applet_padding(true), 14);
    assert_eq!(
        28 + 2 * 14 + 2 * 2,
        60,
        "the symbolic App Library button fixes the Dock width at 60px",
    );
    assert_eq!(parse::<Option<(Vec<String>, Vec<String>)>>(entry, "plugins_wings"), None);

    let center = parse::<Option<Vec<String>>>(entry, "plugins_center").expect("dock has plugins");
    assert_eq!(
        center,
        ["com.clawos.AppList", "com.clawos.PanelDockDivider", "com.clawos.PanelAppButton",],
    );

    // A capsule radius on a vertical bar rounds the whole column away;
    // the dock wants a rounded rectangle.
    let radius = parse::<u32>(entry, "border_radius");
    assert_eq!(radius, 32, "the panel renderer halves this into a 16px radius");
}

/// The widget rail is a floating, nonexclusive right-side surface. Its applet
/// paints the individual glass cards, so the panel wrapper must stay clear.
#[test]
fn widget_rail_layout() {
    use cosmic_panel_config::CosmicPanelBackground;

    let entry = "com.clawos.Panel.WidgetRail";
    assert_eq!(parse::<PanelAnchor>(entry, "anchor"), PanelAnchor::Right);
    assert_eq!(parse::<PanelSize>(entry, "size"), PanelSize::Custom(328));
    assert_eq!(parse::<u16>(entry, "margin"), 24);
    assert!(!parse::<bool>(entry, "exclusive_zone"));
    assert!(!parse::<bool>(entry, "expand_to_edges"));
    assert_eq!(
        parse::<CosmicPanelBackground>(entry, "background"),
        CosmicPanelBackground::Color([0.0, 0.0, 0.0])
    );
    assert_eq!(parse::<f32>(entry, "opacity"), 0.0);
    assert_eq!(
        parse::<Option<Vec<String>>>(entry, "plugins_center"),
        Some(vec!["com.clawos.AppletWidgetRail".to_string()])
    );
}

/// `CosmicPanelContainerConfig::default()` is only consulted when the
/// shipped schema is absent, so drift between the two silently produces
/// a different desktop depending on how ClawOS was installed. Pin them
/// together.
#[test]
fn compiled_defaults_match_shipped_schema() {
    use cosmic_panel_config::CosmicPanelContainerConfig;

    let entries = parse::<Vec<String>>("com.clawos.Panel", "entries");
    let compiled = CosmicPanelContainerConfig::default();

    assert_eq!(
        compiled.config_list.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
        entries,
        "compiled panels differ from the shipped entry list",
    );

    for config in &compiled.config_list {
        let entry = format!("com.clawos.Panel.{}", config.name);
        assert_eq!(
            as_value(&shipped_panel(&entry)),
            as_value(config),
            "{entry} differs from its compiled default",
        );
    }
}

#[test]
fn sample_config_matches_compiled_defaults() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config.ron");
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let sample: cosmic_panel_config::CosmicPanelContainerConfig =
        ron::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));

    assert_eq!(
        as_value(&sample),
        as_value(&cosmic_panel_config::CosmicPanelContainerConfig::default()),
    );
}
