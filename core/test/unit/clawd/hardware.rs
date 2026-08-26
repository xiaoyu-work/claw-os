use super::*;

#[test]
fn lspci_blocks_are_keyed_by_slot() {
    let parsed = parse_lspci_blocks(
        "Slot:\t\"0000:01:00.0\"\nClass:\t\"VGA compatible controller [0300]\"\nDriver:\t\"amdgpu\"\n",
    );
    assert_eq!(parsed["0000:01:00.0"]["driver"], "amdgpu");
}

#[test]
fn hardware_keys_are_normalized() {
    assert_eq!(normalize_key("CPU max MHz:"), "cpu_max_mhz");
}
