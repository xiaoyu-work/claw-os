use super::*;

#[test]
fn parses_zone_table_coordinates() {
    let (latitude, longitude) = parse_iso6709("+404251-0740023").unwrap();
    assert!((latitude - 40.714_166).abs() < 0.001);
    assert!((longitude + 74.006_388).abs() < 0.001);
    assert!(parse_iso6709("invalid").is_none());
}

#[test]
fn maps_accuracy_names_exactly() {
    assert_eq!(accuracy_level("city").unwrap(), 4);
    assert_eq!(accuracy_level("exact").unwrap(), 8);
    assert!(accuracy_level("gps").is_err());
}
