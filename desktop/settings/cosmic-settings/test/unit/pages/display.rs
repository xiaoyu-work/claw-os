#[test]
fn test_cache_rates() {
    let rates: &[u32] = &[10000, 11006, 100004, 100005];

    let mut cached = vec![];

    cache_rates(&mut cached, rates);

    assert_eq!(cached, vec!["10 Hz", "11 Hz", "100.04 Hz", "100.05 Hz"])
}
