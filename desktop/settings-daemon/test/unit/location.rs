use std::path::Path;

#[test]
fn timezone_from_path() {
    let mut path = Path::new("/usr/share/zoneinfo/America/Denver");
    assert_eq!(
        super::timezone_from_path(path),
        String::from("America/Denver")
    );

    path = Path::new("../Pacific/Honolulu");
    assert_eq!(
        super::timezone_from_path(path),
        String::from("Pacific/Honolulu")
    );
}
