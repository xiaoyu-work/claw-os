use crate::Channel;

#[test]
fn volume_balance_to_channel_volumes() {
    // Test conversions to and from a channel
    let channel_map = &[Channel::FL, Channel::FR];
    let inputs = vec![
        ((0.77, Some(0.32)), &[0.45653298, 0.14609055]),
        ((0.77, Some(0.57)), &[0.45653298, 0.2602238]),
        ((0.77, Some(0.68)), &[0.45653298, 0.31044245]),
        ((0.77, Some(0.74)), &[0.45653298, 0.33783442]),
        ((0.77, Some(1.00)), &[0.45653298, 0.45653298]),
        ((0.77, Some(1.32)), &[0.31044242, 0.45653298]),
        ((0.77, Some(1.57)), &[0.19630916, 0.45653298]),
        ((0.77, Some(1.68)), &[0.14609058, 0.45653298]),
        ((0.77, Some(1.74)), &[0.118698575, 0.45653298]),
    ];

    for ((volume, balance), channel_volumes) in inputs {
        let out = super::to_channel_volumes(channel_map, volume, balance);
        assert_eq!(&out, channel_volumes);
        let res = super::from_channel_volumes(&out);
        assert!((volume - res.0).abs() < 0.01, "{} != {}", volume, res.0);
        assert!(
            balance.map_or_else(
                || res.1 == Some(1.0),
                |b| res.1.map_or_else(|| b == 1.0, |r| (b - r).abs() < 0.01)
            ),
            "{:?} != {:?}",
            balance,
            res.1
        );
    }
}
