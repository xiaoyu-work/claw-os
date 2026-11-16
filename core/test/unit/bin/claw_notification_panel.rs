use super::*;

fn snapshot(generation: &str, title: &str) -> View {
    let card = Card {
        version: 1,
        id: 1,
        source_label: "Display data".into(),
        title: title.into(),
        body: vec![],
        activation: Some("default".into()),
        icon: None,
        group_icon: None,
    };
    View {
        generation: Some(generation.into()),
        revision: Some(1),
        cards: BTreeMap::from([(1, card.to_json().unwrap())]),
        preferences: Some(Preferences::new(false)),
        ..View::default()
    }
}

#[test]
fn private_descriptor_never_reuses_a_handle_across_presenter_replacement_or_disconnect() {
    let first = project_handles(&View::default(), snapshot("first", "Before")).unwrap();
    assert_eq!(first.roots, BTreeMap::from([(1, 1)]));
    let unchanged = project_handles(&first, snapshot("first", "Before")).unwrap();
    assert_eq!(unchanged.cards, first.cards);
    let replacement = project_handles(&unchanged, snapshot("second", "After")).unwrap();
    assert_eq!(replacement.roots, BTreeMap::from([(2, 1)]));
    assert!(!replacement.cards.contains_key(&1));
    assert_eq!(Card::from_json(&replacement.cards[&2]).unwrap().id, 2);
    let unavailable = project_handles(&replacement, View::default()).unwrap();
    assert!(unavailable.roots.is_empty());
    let reconnected = project_handles(&unavailable, snapshot("third", "New")).unwrap();
    assert_eq!(reconnected.roots, BTreeMap::from([(3, 1)]));
    assert!(!reconnected.cards.contains_key(&1) && !reconnected.cards.contains_key(&2));
}
