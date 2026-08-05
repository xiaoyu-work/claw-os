//! `cosmic-panel-button` renders whatever the *target* entry named on
//! its command line says — not the applet-registration entry the panel
//! loads it from. That indirection is easy to get wrong: pointing a
//! button at the wrong id silently shows another app's name and icon,
//! and a self-referencing entry would respawn the button instead of
//! launching anything. Pin the shipped wiring.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cosmic::desktop::fde::{DesktopEntry, get_languages_from_env};

fn data_dir(package: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(package)
        .join("data")
}

/// Parse the `[Desktop Entry]` group into a key/value map. Enough for
/// assertions; comments and localized keys are ignored.
fn entry(path: &Path) -> HashMap<String, String> {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('['))
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| !k.contains('['))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[track_caller]
fn assert_valid_desktop(path: &Path) {
    let raw =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    DesktopEntry::from_str(path, &raw, Some(&get_languages_from_env()))
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
}

/// The branding button must present the Claw mark and wordmark, and
/// pressing it must launch the app library rather than another button.
#[test]
fn brand_button_presents_the_claw_mark() {
    let dir = data_dir("claw-panel-brand-button");

    let applet_path = dir.join("com.clawos.PanelBrandButton.desktop");
    assert_valid_desktop(&applet_path);
    let applet = entry(&applet_path);
    assert_eq!(
        applet.get("X-CosmicApplet").map(String::as_str),
        Some("true")
    );

    // The applet registration points at the presentation entry...
    let target = applet.get("Exec").expect("applet entry has Exec");
    let target_id = target
        .strip_prefix("cosmic-panel-button ")
        .expect("applet Exec runs cosmic-panel-button");
    assert_eq!(target_id, "com.clawos.Brand");

    // ...which must exist, or the button panics at startup.
    let brand_path = dir.join(format!("{target_id}.desktop"));
    assert!(brand_path.is_file(), "missing {}", brand_path.display());
    assert_valid_desktop(&brand_path);
    let brand = entry(&brand_path);

    assert_eq!(brand.get("Name").map(String::as_str), Some("ClawOS"));
    assert_eq!(
        brand.get("X-CosmicAppletPresentation").map(String::as_str),
        Some("IconAndText"),
    );

    // Pressing the button runs the target's Exec verbatim. Anything
    // starting another panel button would fork a button, not open the
    // library.
    let exec = brand.get("Exec").expect("brand entry has Exec");
    assert_eq!(exec, "cosmic-app-library");

    // The mark follows the theme, which libcosmic keys off the name.
    let icon = brand.get("Icon").expect("brand entry has Icon");
    assert!(
        icon.ends_with("-symbolic"),
        "brand icon '{icon}' will not be recoloured for dark mode",
    );
    let svg = dir.join("icons/scalable/apps").join(format!("{icon}.svg"));
    assert!(svg.is_file(), "missing icon {}", svg.display());
}

#[test]
fn calendar_query_round_trips_through_freedesktop_parser() {
    let path = data_dir("claw-panel-calendar-button").join("com.clawos.CalendarShortcut.desktop");
    let raw = std::fs::read_to_string(&path).expect("read calendar shortcut");
    let entry = DesktopEntry::from_str(&path, &raw, Some(&get_languages_from_env()))
        .expect("parse calendar shortcut");
    assert_eq!(
        entry.exec(),
        Some(r#"cos app agent overlay --query "Show my calendar for today""#),
    );
}

/// Every shipped panel button follows the same indirection, so a typo
/// in any target id shows the wrong app in the panel.
#[test]
fn panel_buttons_point_at_entries_that_exist() {
    let expected = [
        (
            "claw-panel-brand-button",
            "com.clawos.PanelBrandButton",
            "com.clawos.Brand",
            true,
        ),
        (
            "cosmic-panel-app-button",
            "com.clawos.PanelAppButton",
            "com.clawos.AppLibraryButton",
            true,
        ),
        (
            "cosmic-panel-launcher-button",
            "com.clawos.PanelLauncherButton",
            "com.clawos.Search",
            true,
        ),
        (
            "claw-panel-calendar-button",
            "com.clawos.PanelCalendarButton",
            "com.clawos.CalendarShortcut",
            true,
        ),
        (
            "claw-panel-dock-divider",
            "com.clawos.PanelDockDivider",
            "com.clawos.DockDivider",
            true,
        ),
        (
            "cosmic-panel-workspaces-button",
            "com.clawos.PanelWorkspacesButton",
            "com.clawos.Workspaces",
            false,
        ),
    ];

    for (package, applet_id, target_id, target_is_local) in expected {
        let dir = data_dir(package);
        let path = dir.join(format!("{applet_id}.desktop"));
        assert_valid_desktop(&path);
        let applet = entry(&path);

        assert_eq!(
            applet.get("Exec").map(String::as_str),
            Some(format!("cosmic-panel-button {target_id}").as_str()),
            "{applet_id} points at the wrong entry",
        );

        // A button whose Exec named its own id would respawn itself.
        assert_ne!(applet_id, target_id, "{applet_id} is self-referencing");

        // Dedicated presentation entries live beside the applet
        // registration. The legacy Workspaces target is supplied by
        // the application package instead.
        let target_path = dir.join(format!("{target_id}.desktop"));
        if target_is_local {
            assert!(
                target_path.is_file(),
                "{applet_id} points at missing target {}",
                target_path.display(),
            );
            assert_valid_desktop(&target_path);
            let target = entry(&target_path);
            assert!(
                target.get("Exec").is_some_and(|exec| !exec.is_empty()),
                "{target_id} has no launch command",
            );
            if let Some(icon) = target.get("Icon") {
                if target.get("X-CosmicAppletPresentation").map(String::as_str) == Some("Icon") {
                    assert!(
                        icon.ends_with("-symbolic"),
                        "{target_id} requests Icon presentation but uses non-symbolic '{icon}'",
                    );
                }
            }
        } else {
            let external = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../workspaces/data")
                .join(format!("{target_id}.desktop"));
            assert!(
                external.is_file(),
                "{applet_id} points at missing external target {}",
                external.display(),
            );
            assert_valid_desktop(&external);
        }
    }
}
