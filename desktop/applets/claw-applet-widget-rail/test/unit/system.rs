use super::*;

#[test]
fn parses_cos_resources_response() {
    let summary = parse_resources(
        br#"{
            "disk":{"path":"/home/claw","total_mb":100000,"used_mb":42000,"free_mb":58000},
            "memory":{"total_mb":16000,"used_mb":6000,"available_mb":10000}
        }"#,
    )
    .unwrap();
    assert_eq!(summary.memory.unwrap().percent(), 37);
    assert_eq!(summary.storage.unwrap().percent(), 42);
}

#[test]
fn parses_linux_memory_fallback() {
    let usage =
        parse_meminfo("MemTotal:       8192000 kB\nMemAvailable:   2048000 kB\n").unwrap();
    assert_eq!(usage.total_mb, 8000);
    assert_eq!(usage.used_mb, 6000);
    assert_eq!(usage.percent(), 75);
}

#[test]
fn calculates_cpu_delta() {
    let old = parse_cpu_times("cpu  100 0 50 850 0 0 0 0\n").unwrap();
    let new = parse_cpu_times("cpu  140 0 60 900 0 0 0 0\n").unwrap();
    assert_eq!(cpu_percent(Some(old), Some(new)), Some(50.0));
}

#[test]
fn parses_network_and_ignores_loopback() {
    let raw = "Inter-| Receive | Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n  lo: 100 0 0 0 0 0 0 0 100 0 0 0 0 0 0 0\neth0: 2048 0 0 0 0 0 0 0 4096 0 0 0 0 0 0 0\n";
    let totals = parse_network_totals(raw).unwrap();
    assert_eq!(totals.received, 2048);
    assert_eq!(totals.transmitted, 4096);
}

#[test]
fn calculates_network_rate_from_actual_elapsed_time() {
    assert_eq!(rate(1_000, 4_000, Duration::from_millis(1500)), Some(2_000));
    assert_eq!(rate(1_000, 4_000, Duration::ZERO), None);
}

#[test]
fn parses_df_fallback() {
    let usage = parse_df(
        "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/root 104857600 52428800 52428800 50% /\n",
    )
    .unwrap();
    assert_eq!(usage.total_mb, 102400);
    assert_eq!(usage.used_mb, 51200);
}
