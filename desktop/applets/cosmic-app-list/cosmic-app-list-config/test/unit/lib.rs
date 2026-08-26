use super::*;
use std::path::Path;

#[test]
fn default_favorites_match_the_claw_dock_order() {
    assert_eq!(
        AppListConfig::default().favorites,
        DEFAULT_FAVORITES,
        "compiled defaults must stay aligned with data/default_schema",
    );
}

#[test]
fn every_shipped_default_matches() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let schema_dir = manifest.join("../data/default_schema/com.clawos.AppList/v1");
    let favorites_path = schema_dir.join("favorites");
    let favorites: Vec<String> = ron::from_str(
        &std::fs::read_to_string(&favorites_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", favorites_path.display())),
    )
    .unwrap_or_else(|e| panic!("parse {}: {e}", favorites_path.display()));
    let filter_path = schema_dir.join("filter_top_levels");
    let filter_top_levels: Option<ToplevelFilter> = ron::from_str(
        &std::fs::read_to_string(&filter_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", filter_path.display())),
    )
    .unwrap_or_else(|e| panic!("parse {}: {e}", filter_path.display()));
    let drag_path = schema_dir.join("enable_drag_source");
    let enable_drag_source: bool = ron::from_str(
        &std::fs::read_to_string(&drag_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", drag_path.display())),
    )
    .unwrap_or_else(|e| panic!("parse {}: {e}", drag_path.display()));
    let schema = AppListConfig {
        filter_top_levels,
        favorites,
        enable_drag_source,
    };

    let sample_path = manifest.join("src/config.ron");
    let sample: AppListConfig = ron::from_str(
        &std::fs::read_to_string(&sample_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", sample_path.display())),
    )
    .unwrap_or_else(|e| panic!("parse {}: {e}", sample_path.display()));

    let compiled = AppListConfig::default();
    assert_eq!(schema, compiled);
    assert_eq!(sample, compiled);
}
