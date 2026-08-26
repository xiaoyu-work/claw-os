    use super::*;
    use std::path::{Path, PathBuf};
    use std::{env, fs};
    use tempfile::tempdir;

    struct EnvVarGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &Path) -> Self {
            let original = env::var(key).ok();
            // std::env::{set_var, remove_var} are unsafe on newer toolchains;
            // we limit scope here to the test helper that toggles a single key.
            unsafe { std::env::set_var(key, value) };
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(ref original) = self.original {
                unsafe { std::env::set_var(self.key, original) };
            } else {
                unsafe { std::env::remove_var(self.key) };
            }
        }
    }

    fn load_entry(file_name: &str, contents: &str, locales: &[String]) -> fde::DesktopEntry {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join(file_name);
        fs::write(&path, contents).expect("write desktop file");
        let entry = fde::DesktopEntry::from_path(path, Some(locales)).expect("load desktop file");
        // Ensure directory stays alive until after parsing
        temp.close().expect("close tempdir");
        entry
    }

    #[test]
    fn candidate_generation_covers_common_variants() {
        let ctx = DesktopLookupContext::new("com.example.App.desktop")
            .with_identifier("com-example-App")
            .with_title("Example App");
        let candidates = candidate_desktop_ids(&ctx);

        assert_eq!(candidates.first().unwrap(), "com.example.App.desktop");
        for test in [
            "com.example.App",
            "com-example-App",
            "com_example_App",
            "Example App",
            "Example",
            "App",
        ] {
            assert!(
                candidates
                    .iter()
                    .any(|c| c.to_ascii_lowercase() == test.to_ascii_lowercase()),
            );
        }
    }

    #[test]
    fn startup_wm_class_matching_detects_flatpak_chrome_apps() {
        let temp = tempdir().expect("tempdir");
        let apps_dir = temp.path().join("applications");
        fs::create_dir_all(&apps_dir).expect("create applications dir");

        let desktop_contents = "\
[Desktop Entry]
Version=1.0
Type=Application
Name=Proton Mail
Exec=chromium --app-id=jnpecgipniidlgicjocehkhajgdnjekh
Icon=chrome-jnpecgipniidlgicjocehkhajgdnjekh-Default
StartupWMClass=crx_jnpecgipniidlgicjocehkhajgdnjekh
";
        let desktop_path = apps_dir.join(
            "org.chromium.Chromium.flextop.chrome-jnpecgipniidlgicjocehkhajgdnjekh-Default.desktop",
        );
        fs::write(desktop_path, desktop_contents).expect("write desktop file");

        let _guard = EnvVarGuard::set("XDG_DATA_HOME", temp.path());

        let locales = vec!["en_US.UTF-8".to_string()];
        let mut cache = DesktopEntryCache::new(locales.clone());
        cache.refresh();

        let ctx = DesktopLookupContext::new("crx_jnpecgipniidlgicjocehkhajgdnjekh");
        let resolved = resolve_desktop_entry(&mut cache, &ctx, &DesktopResolveOptions::default());

        assert_eq!(
            resolved.id(),
            "org.chromium.Chromium.flextop.chrome-jnpecgipniidlgicjocehkhajgdnjekh-Default"
        );
    }

    #[test]
    fn exec_basename_matching_handles_vmware() {
        let temp = tempdir().expect("tempdir");
        let apps_dir = temp.path().join("applications");
        fs::create_dir_all(&apps_dir).expect("create applications dir");

        let desktop_contents = "\
[Desktop Entry]\n\
Version=1.0\n\
Type=Application\n\
Name=VMware Workstation\n\
Exec=/usr/bin/vmware %U\n\
Icon=vmware-workstation\n\
";
        let desktop_path = apps_dir.join("vmware-workstation.desktop");
        fs::write(desktop_path, desktop_contents).expect("write desktop file");

        let _guard = EnvVarGuard::set("XDG_DATA_HOME", temp.path());

        let locales = vec!["en_US.UTF-8".to_string()];
        let mut cache = DesktopEntryCache::new(locales.clone());
        cache.refresh();

        let ctx = DesktopLookupContext::new("vmware").with_title("Library — VMware Workstation");

        let resolved = resolve_desktop_entry(&mut cache, &ctx, &DesktopResolveOptions::default());

        assert_eq!(resolved.id(), "vmware-workstation");
    }

    #[test]
    fn proton_fallback_prefers_game_entries() {
        let locales = vec!["en_US.UTF-8".to_string()];
        let entry = load_entry(
            "proton.desktop",
            "[Desktop Entry]\nType=Application\nName=Proton Game\nCategories=Game;Utility;\nExec=proton-game\n",
            &locales,
        );
        let cache = DesktopEntryCache::from_entries(locales.clone(), vec![entry]);
        let ctx = DesktopLookupContext::new("steam_app_default").with_title("Proton Game");

        let resolved = proton_or_wine_fallback(&cache, &ctx).expect("expected proton match");
        let name = resolved
            .name(&locales)
            .expect("name available")
            .into_owned();

        assert_eq!(name, "Proton Game");
    }

    #[test]
    fn proton_fallback_skips_non_games() {
        let locales = vec!["en_US.UTF-8".to_string()];
        let entry = load_entry(
            "tool.desktop",
            "[Desktop Entry]\nType=Application\nName=Proton Tool\nCategories=Utility;\nExec=proton-tool\n",
            &locales,
        );
        let cache = DesktopEntryCache::from_entries(locales, vec![entry]);
        let ctx = DesktopLookupContext::new("steam_app_default").with_title("Proton Tool");

        assert!(proton_or_wine_fallback(&cache, &ctx).is_none());
    }

    #[test]
    fn wine_fallback_matches_executable_titles() {
        let locales = vec!["en_US.UTF-8".to_string()];
        let entry = load_entry(
            "wine.desktop",
            "[Desktop Entry]\nType=Application\nName=Wine Game\nExec=wine-game\n",
            &locales,
        );
        let cache = DesktopEntryCache::from_entries(locales.clone(), vec![entry]);
        let ctx = DesktopLookupContext::new("WINEGAME.EXE").with_title("Wine Game");

        let resolved = proton_or_wine_fallback(&cache, &ctx).expect("expected wine match");
        let name = resolved
            .name(&locales)
            .expect("name available")
            .into_owned();
        assert_eq!(name, "Wine Game");
    }

    #[test]
    fn fallback_entry_uses_title_when_available() {
        let ctx = DesktopLookupContext::new("unknown-app").with_title("Unknown App");
        let entry = fallback_entry(&ctx);

        assert_eq!(entry.id(), "unknown-app");
        assert_eq!(
            entry.name(&["en_US".to_string()]),
            Some(Cow::Owned("Unknown App".to_string()))
        );
    }

    #[test]
    fn desktop_entry_data_prefers_localized_name() {
        let locales = vec!["fr".to_string(), "en_US".to_string()];
        let entry = load_entry(
            "localized.desktop",
            "[Desktop Entry]\nType=Application\nName=Default\nName[fr]=Localisé\nExec=localized\n",
            &locales,
        );
        let data = DesktopEntryData::from_desktop_entry(&locales, entry);

        assert_eq!(data.name, "Localisé");
    }

    #[test]
    fn crx_id_extraction_variants() {
        let id = "cadlkienfkclaiaibeoongdcgmdikeeg"; // 32 chars a..p
        assert_eq!(
            super::extract_crx_id(&format!("chrome-{}-Default", id)),
            Some(id.to_string())
        );
        assert_eq!(
            super::extract_crx_id(&format!("crx_{}", id)),
            Some(id.to_string())
        );
        assert_eq!(super::extract_crx_id(id), Some(id.to_string()));
        // Embedded
        let embedded = format!("org.chromium.Chromium.flextop.chrome-{}-Default", id);
        assert_eq!(super::extract_crx_id(&embedded), Some(id.to_string()));
    }

    #[test]
    fn crx_matcher_by_exec_and_wmclass() {
        use std::fs;
        let id = "cadlkienfkclaiaibeoongdcgmdikeeg";
        let temp = tempdir().expect("tempdir");
        let apps_dir = temp.path().join("applications");
        fs::create_dir_all(&apps_dir).expect("create applications dir");
        let desktop_contents = format!(
            "[Desktop Entry]\nType=Application\nName=ChatGPT\nExec=chromium --app-id={} --profile-directory=Default\nStartupWMClass=crx_{}\nIcon=chrome-{}-Default\n",
            id, id, id
        );
        let desktop_path = apps_dir.join(
            "org.chromium.Chromium.flextop.chrome-cadlkienfkclaiaibeoongdcgmdikeeg-Default.desktop",
        );
        fs::write(&desktop_path, desktop_contents).expect("write desktop file");

        let _guard = EnvVarGuard::set("XDG_DATA_HOME", temp.path());
        let locales = vec!["en_US.UTF-8".to_string()];
        let mut cache = DesktopEntryCache::new(locales.clone());
        cache.refresh();

        let short_id = format!("chrome-{}-Default", id);
        let ctx = DesktopLookupContext::new(short_id);
        let resolved = resolve_desktop_entry(&mut cache, &ctx, &DesktopResolveOptions::default());
        assert!(resolved.icon().is_some());
        assert!(resolved.exec().is_some());
        let expected = format!("crx_{}", id);
        assert_eq!(resolved.startup_wm_class(), Some(expected.as_str()));
    }

    #[test]
    fn crx_extraction_handles_utf8_prefixes() {
        let id = "cadlkienfkclaiaibeoongdcgmdikeeg";
        let prefixed = format!("å{}", id);
        assert_eq!(super::extract_crx_id(&prefixed), Some(id.to_string()));
    }

    #[test]
    fn crx_extraction_ignores_non_ascii_sequences() {
        let id = "cadlkienfkclaiaibeoongdcgmdikeeg";
        let embedded = format!("{id}æøå");

        assert_eq!(super::extract_crx_id(&embedded), Some(id.to_string()));
        assert_eq!(super::extract_crx_id("æøå"), None);
    }
