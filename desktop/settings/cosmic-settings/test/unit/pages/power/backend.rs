use super::*;

#[test]
fn test_can_format_battery_remaining() {
    let cases = [
        (59, "Less than a minute until empty"),
        (300, "5 minutes until empty"),
        (305, "5 minutes until empty"),
        (330, "5 minutes until empty"),
        (360, "6 minutes until empty"),
        (3660, "1 hour and 1 minute until empty"),
        (10800, "3 hours until empty"),
        (969400, "11 days, 5 hours and 16 minutes until empty"),
    ];
    for case in cases {
        let (actual, expected) = case;
        let battery = Battery {
            remaining_duration: actual.seconds(),
            is_charging: false,
            ..Default::default()
        };
        assert_eq!(battery.remaining_time(), expected);
    }
}
