use super::*;
use pop_launcher::PluginSearchResult;

#[test]
fn test_script_calculate_weight() {
    // Test queries for each of the prompt's entries
    let entries = vec![
        (
            "Enter BIOS",
            PluginSearchResult {
                id: 0,
                name: "Enter BIOS".to_string(),
                description: "Reboot into BIOS".to_string(),
                icon: None,
                window: None,
                exec: None,
                category: None,
                keywords: Some(vec![
                    "bios".to_string(),
                    "uefi".to_string(),
                    "reboot".to_string(),
                    "restart".to_string(),
                ]),
            },
        ),
        (
            "Restart",
            PluginSearchResult {
                id: 3,
                name: "Restart".to_string(),
                description: "Reboot the system".to_string(),
                icon: None,
                window: None,
                exec: None,
                category: None,
                keywords: Some(vec![
                    "power".to_string(),
                    "reboot".to_string(),
                    "restart".to_string(),
                ]),
            },
        ),
    ];

    let query_reboot = "reboot";
    let weights_reboot: Vec<f64> = entries
        .iter()
        .map(|(_, entry)| calculate_weight(entry, query_reboot))
        .collect();

    let idx_restart = entries.iter().position(|(n, _)| *n == "Restart").unwrap();
    let idx_bios = entries
        .iter()
        .position(|(n, _)| *n == "Enter BIOS")
        .unwrap();

    assert!(
        weights_reboot[idx_restart] > weights_reboot[idx_bios],
        "Restart should be top for 'reboot', then Enter BIOS"
    );

    assert!(
        weights_reboot[idx_restart] >= 0.85,
        "Restart should be high for 'reboot'"
    );
    assert!(
        weights_reboot[idx_bios] >= 0.85,
        "Enter BIOS should be high for 'reboot'"
    );
}
