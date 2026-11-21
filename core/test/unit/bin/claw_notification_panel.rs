use super::*;

#[tokio::test]
async fn parent_descriptor_orderly_close_retires_the_actual_private_transport() {
    let (server, client) = tokio::net::UnixStream::pair().unwrap();
    let task = tokio::spawn(parent_socket(server));
    let client = Builder::socket(client).p2p().build().await.unwrap();
    let peer = zbus::fdo::PeerProxy::builder(&client)
        .destination("com.yuos.NotificationsSocket")
        .unwrap()
        .path(SOCKET_PATH)
        .unwrap()
        .build()
        .await
        .unwrap();
    peer.ping().await.unwrap();
    drop(peer);
    client.close().await.unwrap();
    let result = tokio::time::timeout(CALL_TIMEOUT, task)
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.is_ok(),
        "an orderly parent close failed transport retirement: {result:?}"
    );
}

#[tokio::test]
async fn parent_descriptor_failed_handshake_is_not_successful_retirement() {
    let (server, client) = tokio::net::UnixStream::pair().unwrap();
    let task = tokio::spawn(parent_socket(server));
    drop(client);
    let result = tokio::time::timeout(CALL_TIMEOUT, task)
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.is_err(),
        "a peer lost before authentication cannot acknowledge retirement"
    );
}

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
