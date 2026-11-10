use super::*;

#[test]
fn session_autostart_honors_availability_without_delegating_execution_to_systemd() {
	let source = include_str!(concat!(
		env!("CARGO_MANIFEST_DIR"),
		"/data/claw-file-manager.desktop"
	));
	let entry = |source: &str| {
		DesktopEntry::from_str::<&str>("claw-file-manager.desktop", source, None).unwrap()
	};
	assert_eq!(
		arguments(&entry(source)).unwrap().unwrap(),
		["--app", "cosmic-files"]
	);
	let alternate = source.replace("--app cosmic-files", "--app alternate-files");
	assert_eq!(
		arguments(&entry(&alternate)).unwrap().unwrap(),
		["--app", "alternate-files"]
	);
	for disabled in [
		format!("{source}\nHidden=true\n"),
		source.replace("OnlyShowIn=COSMIC;", "OnlyShowIn=GNOME;"),
		format!("{source}\nNotShowIn=COSMIC;\n"),
	] {
		assert!(arguments(&entry(&disabled)).unwrap().is_none());
	}
	for invalid in [
		source.replace("X-systemd-skip=true", "X-systemd-skip=false"),
		source.replace(HELPER, "/usr/bin/cosmic-files"),
		source.replace("--app cosmic-files", "--session borrowed"),
	] {
		assert!(arguments(&entry(&invalid)).is_err());
	}
}
