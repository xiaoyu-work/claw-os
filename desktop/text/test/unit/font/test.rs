#[test]
fn test_fonts_load_time() {
    use crate::FontSystem;
    use sys_locale::get_locale;

    #[cfg(not(target_arch = "wasm32"))]
    let now = std::time::Instant::now();

    let mut db = fontdb::Database::new();
    let locale = get_locale().expect("Local available");
    db.load_system_fonts();
    FontSystem::new_with_locale_and_db(locale, db);

    #[cfg(not(target_arch = "wasm32"))]
    println!("Fonts load time {}ms.", now.elapsed().as_millis());
}
