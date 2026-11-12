use super::*;
use serde_json::json;

fn metadata_app(paths: &[&str], panel: bool) -> crate::apps::App {
    let needs = paths.iter().map(|path| json!({
        "verb":"fs.meta", "scope":{"kind":"owner-path","path":path},
        "why":{"en":"Browse this window's declared directory metadata"}
    })).collect::<Vec<_>>();
    let manifest = crate::caps::manifest::Manifest::from_json(&json!({
        "id":"generic-directory-window", "version":"1", "name":{"en":"Directory window"},
        "desktop":{"needs":needs,"panel_applet":panel},
        "operations":{"inspect":{"label":{"en":"Inspect"},"needs":[]}}
    }).to_string()).unwrap();
    crate::apps::App {
        manifest,
        dir: "/usr/lib/cos/apps/generic-directory-window".into(),
        provenance: Err("metadata projection fixture is not installed authority".into()),
    }
}

fn metadata_cap(home: &Path, relative: &str) -> Cap {
    Cap::new(Verb::FS_META, Scope::path(home.join(relative).to_str().unwrap()))
}

#[test]
fn generic_subroot_projection_preserves_home_relative_identity_without_raw_content() {
    let home = Path::new("/home/owner");
    let metadata = metadata_cap(home, "Projects/review/**");
    for panel in [false, true] {
        let app = metadata_app(&["Projects/review/**"], panel);
        let selected = app.manifest.gui_entry(None).unwrap();
        let caps = CapSet::from_caps([
            metadata.clone(),
            Cap::new(Verb::FS_READ, Scope::path("/home/owner/**")),
            Cap::new(Verb::FS_WRITE, Scope::path("/home/owner/**")),
            Cap::new(Verb::FS_WATCH, Scope::path("/home/owner/Projects/review/**")),
            Cap::new(Verb::FS_DELETE, Scope::path("/home/owner/Projects/review/**")),
            Cap::unscoped(Verb::UI_PANEL),
        ]);
        let (cap, relative) = controlled_metadata_scope(&selected, home, &caps).unwrap().unwrap();
        assert_eq!(cap, metadata);
        assert_eq!(relative, Path::new("Projects/review"));
        assert_eq!(home.join(relative), Path::new("/home/owner/Projects/review"));
        assert_eq!(raw_mount_caps(home, &caps, true).unwrap(),
            CapSet::from_caps([Cap::unscoped(Verb::UI_PANEL)]));
        assert!(raw_mount_caps(home, &caps, false).is_err());
    }
}

#[test]
fn metadata_projection_keeps_full_home_and_exact_fixed_subroot_scopes() {
    let home = Path::new("/run/display-home");
    for relative in ["**", "Projects/review/**"] {
        let mut app = metadata_app(&[relative, relative], false);
        let cap = metadata_cap(home, relative);
        let caps = CapSet::from_caps([cap.clone()]);
        let expected = Path::new(relative.strip_suffix("/**").unwrap_or(""));
        let (_, root) = controlled_metadata_scope(
            &app.manifest.gui_entry(None).unwrap(), home, &caps,
        ).unwrap().unwrap();
        assert_eq!(root, expected);
        app.manifest.desktop.as_mut().unwrap().needs = Some(vec![
            serde_json::from_value(json!({
                "verb":"fs.meta","scope":{"kind":"fixed","scope":cap.scope},
                "why":{"en":"Exact absolute directory metadata"}
            })).unwrap(),
        ]);
        assert_eq!(controlled_metadata_scope(
            &app.manifest.gui_entry(None).unwrap(), home, &caps,
        ).unwrap().unwrap().1, expected);
    }
}

#[test]
fn one_view_refuses_conflicting_scopes_and_broad_or_optional_substitutes() {
    let home = Path::new("/home/owner");
    for paths in [
        vec!["Projects/**", "Projects/review/**"],
        vec!["Projects/**", "Other/**"],
        vec!["**", "Projects/**"],
    ] {
        let app = metadata_app(&paths, false);
        let caps = CapSet::from_caps(paths.iter().map(|path| metadata_cap(home, path)));
        assert!(controlled_metadata_scope(&app.manifest.gui_entry(None).unwrap(), home, &caps).is_err());
    }
    let mut app = metadata_app(&["Projects/**"], false);
    let subroot = CapSet::from_caps([metadata_cap(home, "Projects/**")]);
    let broad = CapSet::from_caps([metadata_cap(home, "**")]);
    assert!(controlled_metadata_scope(&app.manifest.gui_entry(None).unwrap(), home, &broad).is_err());
    app.manifest.desktop.as_mut().unwrap().needs.as_mut().unwrap()[0].when =
        Some(serde_json::from_value(json!({"kind":"arg-present","arg":"path"})).unwrap());
    assert!(controlled_metadata_scope(&app.manifest.gui_entry(None).unwrap(), home, &subroot).unwrap().is_none());
    app.manifest.desktop.as_mut().unwrap().needs.as_mut().unwrap()[0].when = None;
    let desktop = app.manifest.desktop.as_mut().unwrap();
    desktop.optional_needs = desktop.needs.take().unwrap();
    desktop.needs = Some(Vec::new());
    assert!(controlled_metadata_scope(&app.manifest.gui_entry(None).unwrap(), home, &subroot).unwrap().is_none());
    assert!(validate_optional_mounts(&app, None, home, &subroot).is_err());
    app.manifest.desktop.as_mut().unwrap().needs = None;
    assert!(controlled_metadata_scope(&app.manifest.gui_entry(None).unwrap(), home, &subroot).unwrap().is_none());
}

#[test]
fn metadata_subroot_refuses_bare_glob_escaping_and_noncanonical_scopes() {
    let home = Path::new("/home/owner");
    for path in [
        "/home/owner",
        "/home/owner//**",
        "/home/owner/./**",
        "/home/owner/../owner/**",
        "/home/owner/Projects",
        "/home/owner/Projects/*/**",
        "/home/owner/Projects/../Other/**",
        "/home/owner/Projects/./review/**",
        "/home/owner//Projects/**",
        "/home/owner/Projects//**",
        "/home/owner/Projects/[ab]/**",
    ] {
        let mut app = metadata_app(&[], false);
        let cap = Cap::new(Verb::FS_META, Scope::path(path));
        app.manifest.desktop.as_mut().unwrap().needs = Some(vec![
            serde_json::from_value(json!({
                "verb":"fs.meta","scope":{"kind":"fixed","scope":cap.scope},
                "why":{"en":"Invalid recursive directory"}
            })).unwrap(),
        ]);
        assert!(controlled_metadata_scope(
            &app.manifest.gui_entry(None).unwrap(), home, &CapSet::from_caps([cap]),
        ).is_err(), "unsupported metadata scope {path}");
    }
}

#[test]
fn subroot_preflight_requires_a_real_unaliased_unprotected_directory() {
    std::fs::create_dir_all("build").unwrap();
    let directory = tempfile::Builder::new().prefix("gui-metadata-").tempdir_in("build").unwrap();
    let home = directory.path().canonicalize().unwrap();
    std::fs::create_dir_all(home.join("Projects/review")).unwrap();
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(home.join("plain-file"), b"not a browse root").unwrap();
    std::os::unix::fs::symlink("Projects", home.join("alias")).unwrap();
    for relative in ["missing", "plain-file", "alias", "alias/review", ".ssh"] {
        let scope = format!("{relative}/**");
        let app = metadata_app(&[&scope], false);
        let caps = CapSet::from_caps([metadata_cap(&home, &scope)]);
        assert!(validate_optional_mounts(&app, None, &home, &caps).is_err(), "{relative}");
        assert!(raw_mount_caps(&home, &caps, false).is_err());
    }
    let mut app = metadata_app(&["Projects/review/**"], false);
    let mut caps = CapSet::from_caps([metadata_cap(&home, "Projects/review/**")]);
    for verb in [Verb::FS_READ, Verb::FS_WRITE, Verb::FS_WATCH, Verb::FS_DELETE] {
        caps.insert(Cap::new(verb, Scope::path(home.join("**").to_str().unwrap())));
    }
    assert!(validate_optional_mounts(&app, None, &home, &caps).is_ok());
    assert!(raw_mount_caps(&home, &caps, true).unwrap().is_empty());
    app.manifest.desktop.as_mut().unwrap().needs.as_mut().unwrap().push(
        serde_json::from_value(json!({
            "verb":"fs.watch","scope":{"kind":"owner-path","path":"Projects/review/**"},
            "why":{"en":"Unsupported mandatory native watch"}
        })).unwrap(),
    );
    assert!(validate_optional_mounts(&app, None, &home, &caps).is_err());
}
