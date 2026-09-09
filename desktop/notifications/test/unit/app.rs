use super::*;

#[tokio::test]
async fn visible_model_keeps_presentation_and_distinguishes_expiry_from_dismissal() {
    let (tx, mut rx) = mpsc::channel(10);
    let mut ui = CosmicNotifications {
        core: Core::default(),
        active_surface: false,
        autosize_id: iced::id::Id::new("fixture"),
        window_id: SurfaceId::unique(),
        cards: vec![],
        hidden: VecDeque::new(),
        notifications_id: id::Id::new("fixture"),
        notifications_tx: Some(tx),
        config: NotificationsConfig::default(),
        dock_config: CosmicPanelConfig::default(),
        panel_config: CosmicPanelConfig::default(),
        anchor: None,
    };
    let notification = Notification::new(
        "Label",
        1,
        "icon-name",
        "Title",
        "&lt;b&gt;plain&lt;/b&gt;",
        vec!["default", "Acknowledge"],
        std::collections::HashMap::new(),
        0,
    );
    drop(ui.push_notification(notification.clone()));
    assert_eq!(ui.cards, vec![notification.clone()]);
    let mut replacement = notification.clone();
    replacement.time += Duration::from_secs(1);
    replacement.summary = "Updated".into();
    drop(ui.replace_notification(replacement.clone()));
    drop(ui.update(Message::Timeout(1, notification.time)));
    assert_eq!(
        ui.cards,
        vec![replacement.clone()],
        "stale timers cannot expire replacements"
    );
    drop(ui.update(Message::Timeout(1, replacement.time)));
    assert!(ui.cards.is_empty());
    assert_eq!(ui.hidden.front(), Some(&replacement));
    assert!(
        rx.try_recv().is_err(),
        "popup expiry does not acknowledge durable activity"
    );
    drop(ui.close(1, CloseReason::CloseNotification));
    assert!(matches!(
        rx.recv().await,
        Some(notifications::Input::Closed(
            1,
            CloseReason::CloseNotification
        ))
    ));
    assert!(
        rx.try_recv().is_err(),
        "close emits one reason, not a second user dismissal"
    );
}
