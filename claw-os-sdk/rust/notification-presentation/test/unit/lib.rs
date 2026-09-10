use super::*;
use serde_json::json;

fn card() -> Card {
    Card {
        version: VERSION,
        id: 1,
        source_label: "An arbitrary presenter".into(),
        title: "Title".into(),
        body: vec![TextRun {
            text: "Body".into(),
            bold: true,
            ..TextRun::default()
        }],
        activation: Some("default".into()),
        icon: Some(Icon::Rgba {
            width: 1,
            height: 1,
            pixels: vec![0, 0, 0, 255],
        }),
        group_icon: None,
    }
}

#[test]
fn roundtrips_presentation_without_a_renderer_or_product_crate() {
    let value = card();
    assert_eq!(Card::from_json(&value.to_json().unwrap()).unwrap(), value);
    for muted in [false, true] {
        let preferences = Preferences::new(muted);
        assert_eq!(
            Preferences::from_json(&preferences.to_json().unwrap()).unwrap(),
            preferences,
        );
    }
}

#[test]
fn rejects_authority_and_product_configuration_fields() {
    for (key, value) in [
        ("owner", json!(1000)),
        ("app_id", json!("cosmic-notifications")),
        ("trusted", json!(true)),
        ("capabilities", json!(["ui.notify"])),
        ("contract_digest", json!("printed-contract-is-not-proof")),
        ("reviewed", json!(true)),
        ("permission_choice", json!("allow")),
        ("purpose", json!("new purpose field")),
        ("plugin", json!(true)),
        ("yes", json!(true)),
        ("anchor", json!("Top")),
        ("max_notifications", json!(3)),
    ] {
        let mut payload = serde_json::to_value(card()).unwrap();
        payload
            .as_object_mut()
            .unwrap()
            .insert(key.into(), value.clone());
        assert!(Card::from_json(&payload.to_string()).is_err(), "{key}");
        let mut preferences = json!({"version": 1, "muted": true});
        preferences
            .as_object_mut()
            .unwrap()
            .insert(key.into(), value);
        assert!(
            Preferences::from_json(&preferences.to_string()).is_err(),
            "{key}"
        );
    }
    assert!(Preferences::from_json(r#"{"version":1,"muted":false,"muted":true}"#).is_err());
}

#[test]
fn review_claim_text_remains_only_display_content() {
    let mut value = card();
    value.body[0].text = r#"{"contract_digest":"printed-value","reviewed":true}"#.into();
    assert_eq!(Card::from_json(&value.to_json().unwrap()).unwrap(), value);
    assert!(Preferences::from_json(&value.body[0].text).is_err());
}

#[test]
fn checks_versions_handles_actions_and_text_bounds() {
    let mut value = card();
    value.version = 2;
    assert!(value.to_json().is_err());
    assert!(Preferences::from_json(r#"{"version":2,"muted":false}"#).is_err());
    value.version = VERSION;
    value.id = 0;
    assert!(value.to_json().is_err());
    value.id = 1;
    value.body[0].text = "x".repeat(MAX_TEXT_BYTES);
    assert!(value.to_json().is_ok());
    value.body[0].text.push('x');
    assert!(value.to_json().is_err());
    value.body = vec![TextRun::default(); MAX_RUNS + 1];
    assert!(value.to_json().is_err());
    assert!(validate_action(1, &"x".repeat(4096)).is_ok());
    assert!(validate_action(1, &"x".repeat(4097)).is_err());
    assert!(validate_action(0, "default").is_err());
    assert!(Card::from_json(&" ".repeat(MAX_PACKET_BYTES + 1)).is_err());
    assert!(Preferences::from_json(&" ".repeat(MAX_PREFERENCES_BYTES + 1)).is_err());
}

#[test]
fn icons_never_expose_host_paths_and_have_exact_bounded_dimensions() {
    let image = Icon::Rgba {
        width: 2,
        height: 1,
        pixels: vec![0; 8],
    };
    assert!(image.validate().is_ok());
    for image in [
        Icon::Rgba {
            width: 0,
            height: 1,
            pixels: vec![],
        },
        Icon::Rgba {
            width: 2,
            height: 1,
            pixels: vec![0; 7],
        },
        Icon::Rgba {
            width: u32::MAX,
            height: u32::MAX,
            pixels: vec![],
        },
        Icon::Mask {
            width: 2,
            height: 1,
            alpha: vec![0],
        },
        Icon::Mask {
            width: MAX_IMAGE_DIMENSION + 1,
            height: 1,
            alpha: vec![0; MAX_IMAGE_DIMENSION as usize + 1],
        },
    ] {
        assert!(image.validate().is_err());
    }
    assert!(Icon::Rgba {
        width: MAX_IMAGE_DIMENSION,
        height: MAX_IMAGE_DIMENSION,
        pixels: vec![0; MAX_IMAGE_BYTES],
    }
    .validate()
    .is_ok());
    assert!(Icon::Mask {
        width: MAX_IMAGE_DIMENSION,
        height: MAX_IMAGE_DIMENSION,
        alpha: vec![0; MAX_IMAGE_BYTES / 4],
    }
    .validate()
    .is_ok());
    assert!(
        Icon::Rgba {
            width: 1,
            height: (MAX_IMAGE_BYTES / 4) as u32,
            pixels: vec![0; MAX_IMAGE_BYTES],
        }
        .validate()
        .is_err(),
        "a byte bound alone does not bound either image dimension"
    );
}

#[test]
fn encoded_and_named_images_cannot_enter_any_os_decoder() {
    for image in [
        json!({"kind":"encoded","svg":true,"symbolic":false,"bytes":b"<svg><image href=\"/private/image.png\"/></svg>".to_vec()}),
        json!({"kind":"encoded","svg":false,"symbolic":false,"bytes":[137,80,78,71,13,10,26,10]}),
        json!({"kind":"theme","name":"image-supplied-by-presenter"}),
        json!({"kind":"rgba","width":1,"height":1,"pixels":[0,0,0,255],"path":"/private/image.png"}),
        json!({"kind":"mask","width":1,"height":1,"alpha":[255],"uri":"https://example.invalid/image"}),
    ] {
        for field in ["icon", "group_icon"] {
            let mut value = serde_json::to_value(card()).unwrap();
            value[field] = image.clone();
            assert!(
                Card::from_json(&value.to_string()).is_err(),
                "{field}: {image}"
            );
        }
    }
}

#[test]
fn normalized_symbolic_masks_roundtrip_without_encoded_images() {
    let mut value = card();
    value.icon = Some(Icon::Mask {
        width: 3,
        height: 1,
        alpha: vec![0, 127, 255],
    });
    assert_eq!(Card::from_json(&value.to_json().unwrap()).unwrap(), value);
}
