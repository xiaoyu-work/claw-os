use super::*;
use serde_json::{json, Value};

fn manifest() -> Value {
    json!({
        "id":"demo","version":"1","name":{"en":"Demo"},
        "operations":{"write":{"label":{"en":"Write"},"args":[
            {"name":"path","kind":"path","required":true},
            {"name":"content","kind":"text","binding":"flag"}
        ],"effects":[{"kind":"update","label":{"en":"Write file"},"target_arg":"path"}]}}
    })
}

#[test]
fn effect_declarations_are_optional_and_default_recovery_to_unknown() {
    let value = Manifest::from_json(&manifest().to_string()).unwrap();
    assert_eq!(
        value.operations["write"].effects[0].recovery,
        EffectRecovery::Unknown
    );
    let mut legacy = manifest();
    legacy["operations"]["write"]
        .as_object_mut()
        .unwrap()
        .remove("effects");
    let value = Manifest::from_json(&legacy.to_string()).unwrap();
    assert!(value.operations["write"].effects.is_empty());
}

#[test]
fn effect_targets_cannot_expose_arbitrary_text_or_missing_arguments() {
    for target in ["missing", "content"] {
        let mut value = manifest();
        value["operations"]["write"]["effects"][0]["target_arg"] = json!(target);
        assert!(Manifest::from_json(&value.to_string()).is_err());
    }
}

#[test]
fn effect_shapes_labels_and_recovery_modes_are_closed_and_bounded() {
    for (field, bad) in [
        ("kind", json!("guaranteed-safe")),
        ("recovery", json!("not_applicable")),
        ("owner_uid", json!(0)),
        ("label", json!({"en":"x".repeat(513)})),
        ("label", json!({"en":"bad\nlabel"})),
        ("label", json!({"zh-CN":"missing English"})),
    ] {
        let mut value = manifest();
        value["operations"]["write"]["effects"][0][field] = bad;
        assert!(Manifest::from_json(&value.to_string()).is_err(), "{field}");
    }
    let mut value = manifest();
    let effect = value["operations"]["write"]["effects"][0].clone();
    value["operations"]["write"]["effects"] = json!(vec![effect; 17]);
    assert!(Manifest::from_json(&value.to_string()).is_err());
    let mut value = manifest();
    value["operations"]["write"]["effects"][0]["kind"] = json!("read");
    value["operations"]["write"]["effects"][0]["recovery"] = json!("irreversible");
    assert!(Manifest::from_json(&value.to_string()).is_err());
}
