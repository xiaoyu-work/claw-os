// SPDX-License-Identifier: GPL-3.0-only

mod support;

use smithay::wayland::selection::SelectionTarget::{self, Clipboard, Primary};
use support::{Access, CASES, MIME, Peer, Protocol, Server, default_allows, read_pipe};

#[test]
fn default_hook_preserves_access_without_an_override() {
    let server = Server::new();
    let denied = Access::new(false);
    let peer = Peer::new(&server, Protocol::Core, denied.clone());
    assert!(default_allows(&peer, Clipboard));
    assert!(default_allows(&peer, Primary));
    assert_eq!(denied.check_count(), 0);
}

#[test]
fn denied_recipients_get_no_initial_or_updated_offers_or_mime() {
    for &(protocol, target) in CASES {
        let server = Server::new();
        server.set_selection(target, b"initial private payload");
        let access = Access::new(false);
        let mut peer = Peer::new(&server, protocol, access.clone());
        server.focus(&peer);
        peer.roundtrip();
        assert_eq!(peer.offer_count(), 0, "{protocol:?}/{target:?}");
        assert_eq!(peer.mime_count(), 0, "{protocol:?}/{target:?}");

        server.set_selection(target, b"updated private payload");
        peer.roundtrip();
        assert_eq!(peer.offer_count(), 0, "{protocol:?}/{target:?}");
        assert_eq!(peer.mime_count(), 0, "{protocol:?}/{target:?}");
        assert!(access.check_count() >= 2, "{protocol:?}/{target:?}");
        assert_eq!(server.payload_writes(), 0);

        access.set(target, true);
        server.set_selection(target, b"newly permitted payload");
        peer.roundtrip();
        let offer = peer.offer(target);
        assert_eq!(peer.mime_count(), 1, "{protocol:?}/{target:?}");
        let received = peer.receive(&offer, MIME);
        assert_eq!(read_pipe(received), b"newly permitted payload");
    }
}

#[test]
fn held_offers_recheck_before_compositor_payload_delivery() {
    for &(protocol, target) in CASES {
        let server = Server::new();
        let access = Access::new(true);
        let mut peer = Peer::new(&server, protocol, access.clone());
        server.focus(&peer);
        server.set_selection(target, b"server payload");
        peer.roundtrip();
        let offer = peer.offer(target);
        let allowed = peer.receive(&offer, MIME);
        assert_eq!(read_pipe(allowed), b"server payload");
        assert_eq!(server.payload_writes(), 1);

        access.set(target, false);
        let checks = access.check_count();
        let denied = peer.receive(&offer, MIME);
        assert!(read_pipe(denied).is_empty(), "{protocol:?}/{target:?}");
        assert_eq!(server.payload_writes(), 1, "{protocol:?}/{target:?}");
        assert_eq!(access.check_count(), checks + 1);
    }
}

#[test]
fn denied_recipients_get_no_client_source_offers_or_mime() {
    for &(protocol, target) in CASES {
        let server = Server::new();
        let mut source = Peer::new(&server, protocol, Access::new(false));
        source.publish(&server, target, b"initial client payload");
        let access = Access::new(false);
        let mut recipient = Peer::new(&server, protocol, access.clone());
        server.focus(&recipient);
        recipient.roundtrip();
        assert_eq!(recipient.offer_count(), 0, "{protocol:?}/{target:?}");
        assert_eq!(recipient.mime_count(), 0, "{protocol:?}/{target:?}");

        source.publish(&server, target, b"updated client payload");
        server.focus(&recipient);
        recipient.roundtrip();
        assert_eq!(recipient.offer_count(), 0, "{protocol:?}/{target:?}");
        assert_eq!(recipient.mime_count(), 0, "{protocol:?}/{target:?}");
        assert!(access.check_count() >= 2);
        assert_eq!(source.source_sends(), 0);

        access.set(target, true);
        source.publish(&server, target, b"permitted client payload");
        server.focus(&recipient);
        recipient.roundtrip();
        let offer = recipient.offer(target);
        let received = recipient.receive(&offer, MIME);
        source.roundtrip();
        assert_eq!(read_pipe(received), b"permitted client payload");
        assert_eq!(source.source_sends(), 1);
    }
}

#[test]
fn held_offers_recheck_before_forwarding_client_source_fds() {
    for &(protocol, target) in CASES {
        let server = Server::new();
        let mut source = Peer::new(&server, protocol, Access::new(false));
        let access = Access::new(true);
        let mut recipient = Peer::new(&server, protocol, access.clone());
        source.publish(&server, target, b"client payload");
        server.focus(&recipient);
        recipient.roundtrip();
        let offer = recipient.offer(target);
        let allowed = recipient.receive(&offer, MIME);
        source.roundtrip();
        assert_eq!(read_pipe(allowed), b"client payload");
        assert_eq!(source.source_sends(), 1);

        access.set(target, false);
        let denied = recipient.receive(&offer, MIME);
        source.roundtrip();
        assert!(read_pipe(denied).is_empty(), "{protocol:?}/{target:?}");
        assert_eq!(source.source_sends(), 1, "{protocol:?}/{target:?}");
        assert_eq!(server.payload_writes(), 0);
    }
}

#[test]
fn data_control_offers_retain_the_server_selected_target() {
    for protocol in [Protocol::Wlr, Protocol::Ext] {
        for client_source in [false, true] {
            let server = Server::new();
            let mut source = Peer::new(&server, protocol, Access::new(false));
            let access = Access::new(false);
            access.set(Clipboard, true);
            let mut recipient = Peer::new(&server, protocol, access.clone());
            if client_source {
                source.publish(&server, Clipboard, b"clipboard target");
                source.publish(&server, Primary, b"primary target");
            } else {
                server.set_selection(Clipboard, b"clipboard target");
                server.set_selection(Primary, b"primary target");
            }
            recipient.roundtrip();
            assert!(recipient.has_offer(Clipboard));
            assert!(!recipient.has_offer(Primary));
            assert_eq!(recipient.mime_count(), 1);
            let clipboard_offer = recipient.offer(Clipboard);

            access.set(Clipboard, false);
            access.set(Primary, true);
            let denied = recipient.receive(&clipboard_offer, MIME);
            source.roundtrip();
            assert!(read_pipe(denied).is_empty());
            assert_eq!(source.source_sends(), 0);
            assert_eq!(server.payload_writes(), 0);

            if client_source {
                source.publish(&server, Primary, b"permitted primary target");
            } else {
                server.set_selection(Primary, b"permitted primary target");
            }
            recipient.roundtrip();
            let primary_offer = recipient.offer(Primary);
            let allowed = recipient.receive(&primary_offer, MIME);
            source.roundtrip();
            assert_eq!(read_pipe(allowed), b"permitted primary target");
            access.set(Primary, false);
            access.set(Clipboard, true);
            let denied = recipient.receive(&primary_offer, MIME);
            source.roundtrip();
            assert!(read_pipe(denied).is_empty());
        }
    }
}

#[test]
fn invalid_mime_is_rejected_before_the_fresh_policy_check() {
    for &(protocol, target) in CASES {
        let server = Server::new();
        let access = Access::new(true);
        let mut recipient = Peer::new(&server, protocol, access.clone());
        server.focus(&recipient);
        server.set_selection(target, b"valid MIME payload");
        recipient.roundtrip();
        let offer = recipient.offer(target);
        let checks = access.check_count();
        let invalid = recipient.receive(&offer, "application/not-offered");
        assert!(read_pipe(invalid).is_empty());
        assert_eq!(access.check_count(), checks);
        assert_eq!(server.payload_writes(), 0);
    }
}

#[test]
fn denied_clipboard_reads_do_not_disable_dnd_offers_or_transfers() {
    let server = Server::new();
    server.set_selection(Clipboard, b"not a drag");
    let mut source = Peer::new(&server, Protocol::Core, Access::new(false));
    let denied = Access::new(false);
    let mut recipient = Peer::new(&server, Protocol::Core, denied.clone());
    server.focus(&recipient);
    recipient.roundtrip();
    assert_eq!(recipient.offer_count(), 0);
    assert!(denied.check_count() > 0);

    source.start_drag(&server, b"independent drag payload");
    assert_eq!(server.drag_starts(), 1);
    server.drag_to(&recipient);
    recipient.roundtrip();
    let offer = recipient.dnd_offer();
    assert_eq!(recipient.mime_count(), 1);
    let checks = denied.check_count();
    let received = recipient.receive(&offer, MIME);
    source.roundtrip();
    assert_eq!(read_pipe(received), b"independent drag payload");
    assert_eq!(source.source_sends(), 1);
    assert_eq!(denied.check_count(), checks);
    assert_eq!(server.payload_writes(), 0);
}

fn initial_notifications(
    protocol: Protocol,
    target: SelectionTarget,
    visible: bool,
) -> Vec<(SelectionTarget, bool)> {
    match protocol {
        Protocol::Core | Protocol::Primary => Vec::new(),
        Protocol::Wlr | Protocol::Ext => vec![
            (Clipboard, visible && target == Clipboard),
            (Primary, visible && target == Primary),
        ],
    }
}

fn focus_notifications(
    protocol: Protocol,
    target: SelectionTarget,
    visible: bool,
) -> Vec<(SelectionTarget, bool)> {
    match protocol {
        Protocol::Core | Protocol::Primary => vec![(target, visible)],
        Protocol::Wlr | Protocol::Ext => Vec::new(),
    }
}

#[test]
fn denied_selection_initial_and_focus_notifications_are_content_independent() {
    for &(protocol, target) in CASES {
        for populated in [false, true] {
            let server = Server::new();
            if populated {
                server.set_selection(target, b"private initial selection");
            }
            let mut peer = Peer::new(&server, protocol, Access::new(false));
            assert_eq!(
                peer.take_selection_events(),
                initial_notifications(protocol, target, false),
                "initial {protocol:?}/{target:?}, populated={populated}"
            );
            server.focus(&peer);
            peer.roundtrip();
            assert_eq!(
                peer.take_selection_events(),
                focus_notifications(protocol, target, false),
                "focus {protocol:?}/{target:?}, populated={populated}"
            );
            assert_eq!(peer.offer_count(), 0);
            assert_eq!(peer.mime_count(), 0);

            server.unfocus();
            peer.roundtrip();
            peer.take_selection_events();
            server.focus(&peer);
            peer.roundtrip();
            assert_eq!(
                peer.take_selection_events(),
                focus_notifications(protocol, target, false),
                "refocus {protocol:?}/{target:?}, populated={populated}"
            );
        }
    }
}

#[test]
fn denied_selection_updates_have_no_private_notifications() {
    for &(protocol, target) in CASES {
        for populated in [false, true] {
            for client_source in [false, true] {
                let server = Server::new();
                let mut source = Peer::new(&server, Protocol::Wlr, Access::new(false));
                if populated {
                    if client_source {
                        source.publish(&server, target, b"private initial selection");
                    } else {
                        server.set_selection(target, b"private initial selection");
                    }
                }
                let mut peer = Peer::new(&server, protocol, Access::new(false));
                assert_eq!(
                    peer.take_selection_events(),
                    initial_notifications(protocol, target, false)
                );
                server.focus(&peer);
                peer.roundtrip();
                assert_eq!(
                    peer.take_selection_events(),
                    focus_notifications(protocol, target, false)
                );
                let mut observer = Peer::new(&server, Protocol::Wlr, Access::new(true));
                assert_eq!(
                    observer.take_selection_events(),
                    initial_notifications(Protocol::Wlr, target, populated)
                );

                for contents in [None, Some(b"first".as_slice()), Some(b"second"), None, None] {
                    match (client_source, contents) {
                        (true, Some(bytes)) => source.publish(&server, target, bytes),
                        (true, None) => source.clear_selection(&server, target),
                        (false, Some(bytes)) => server.set_selection(target, bytes),
                        (false, None) => server.clear_selection(target),
                    }
                    peer.roundtrip();
                    observer.roundtrip();
                    assert_eq!(
                        observer.take_selection_events(),
                        vec![(target, contents.is_some())],
                        "allowed observer must see the actual update"
                    );
                    assert!(
                        peer.take_selection_events().is_empty(),
                        "private update leaked for {protocol:?}/{target:?}, populated={populated}, client_source={client_source}"
                    );
                    assert_eq!(peer.offer_count(), 0);
                    assert_eq!(peer.mime_count(), 0);
                }
                assert_eq!(source.source_sends(), 0);
                assert_eq!(server.payload_writes(), 0);
            }
        }
    }
}

#[test]
fn allowed_selection_notifications_keep_upstream_behavior() {
    for &(protocol, target) in CASES {
        for populated in [false, true] {
            let server = Server::new();
            if populated {
                server.set_selection(target, b"permitted initial selection");
            }
            let mut peer = Peer::new(&server, protocol, Access::new(true));
            assert_eq!(
                peer.take_selection_events(),
                initial_notifications(protocol, target, populated)
            );
            server.focus(&peer);
            peer.roundtrip();
            assert_eq!(
                peer.take_selection_events(),
                focus_notifications(protocol, target, populated)
            );

            for visible in [false, true, true, false, false] {
                if visible {
                    server.set_selection(target, b"permitted update");
                } else {
                    server.clear_selection(target);
                }
                peer.roundtrip();
                assert_eq!(
                    peer.take_selection_events(),
                    vec![(target, visible)],
                    "allowed update {protocol:?}/{target:?}"
                );
            }
        }
    }
}
