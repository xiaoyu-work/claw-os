// SPDX-License-Identifier: GPL-3.0-only

use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Write},
    os::fd::{AsFd, OwnedFd},
    sync::Arc,
    time::Instant,
};

use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    pipe::{PipeFlags, pipe_with},
};
use smithay::{
    reexports::wayland_server::Client as ServerClient, wayland::selection::SelectionTarget,
};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
    backend::ObjectId,
    delegate_noop, event_created_child,
    protocol::{
        wl_callback, wl_compositor, wl_data_device, wl_data_device_manager, wl_data_offer,
        wl_data_source, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_surface,
    },
};
use wayland_protocols::{
    ext::data_control::v1::client::{
        ext_data_control_device_v1 as ext_device, ext_data_control_manager_v1 as ext_manager,
        ext_data_control_offer_v1 as ext_offer, ext_data_control_source_v1 as ext_source,
    },
    wp::primary_selection::zv1::client::{
        zwp_primary_selection_device_manager_v1 as primary_manager,
        zwp_primary_selection_device_v1 as primary_device,
        zwp_primary_selection_offer_v1 as primary_offer,
        zwp_primary_selection_source_v1 as primary_source,
    },
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1 as wlr_device, zwlr_data_control_manager_v1 as wlr_manager,
    zwlr_data_control_offer_v1 as wlr_offer, zwlr_data_control_source_v1 as wlr_source,
};

use super::{Access, DEADLINE, MIME, Server};

#[derive(Clone, Copy, Debug)]
pub enum Protocol {
    Core,
    Primary,
    Wlr,
    Ext,
}

enum Device {
    Core(
        wl_data_device_manager::WlDataDeviceManager,
        wl_data_device::WlDataDevice,
    ),
    Primary(
        primary_manager::ZwpPrimarySelectionDeviceManagerV1,
        primary_device::ZwpPrimarySelectionDeviceV1,
    ),
    Wlr(
        wlr_manager::ZwlrDataControlManagerV1,
        wlr_device::ZwlrDataControlDeviceV1,
    ),
    Ext(
        ext_manager::ExtDataControlManagerV1,
        ext_device::ExtDataControlDeviceV1,
    ),
}

#[derive(Clone, Debug)]
pub enum Offer {
    Core(wl_data_offer::WlDataOffer),
    Primary(primary_offer::ZwpPrimarySelectionOfferV1),
    Wlr(wlr_offer::ZwlrDataControlOfferV1),
    Ext(ext_offer::ExtDataControlOfferV1),
}

impl Offer {
    fn id(&self) -> ObjectId {
        match self {
            Self::Core(offer) => offer.id(),
            Self::Primary(offer) => offer.id(),
            Self::Wlr(offer) => offer.id(),
            Self::Ext(offer) => offer.id(),
        }
    }

    fn receive(&self, mime: &str, fd: &OwnedFd) {
        match self {
            Self::Core(offer) => offer.receive(mime.to_string(), fd.as_fd()),
            Self::Primary(offer) => offer.receive(mime.to_string(), fd.as_fd()),
            Self::Wlr(offer) => offer.receive(mime.to_string(), fd.as_fd()),
            Self::Ext(offer) => offer.receive(mime.to_string(), fd.as_fd()),
        }
    }
}

enum Source {
    Core(wl_data_source::WlDataSource),
    Primary(primary_source::ZwpPrimarySelectionSourceV1),
    Wlr(wlr_source::ZwlrDataControlSourceV1),
    Ext(ext_source::ExtDataControlSourceV1),
}

impl Source {
    fn id(&self) -> ObjectId {
        match self {
            Self::Core(source) => source.id(),
            Self::Primary(source) => source.id(),
            Self::Wlr(source) => source.id(),
            Self::Ext(source) => source.id(),
        }
    }
}

struct SourcePayload(Vec<u8>);

#[derive(Default)]
struct Events {
    globals: HashMap<String, (u32, u32)>,
    synced: bool,
    offers: HashMap<ObjectId, Vec<String>>,
    clipboard: Option<Offer>,
    primary: Option<Offer>,
    selection_events: Vec<(SelectionTarget, bool)>,
    source_sends: usize,
    dnd: Option<(wl_data_offer::WlDataOffer, u32)>,
}

impl Events {
    fn add_offer(&mut self, offer: Offer) {
        assert!(self.offers.insert(offer.id(), Vec::new()).is_none());
    }

    fn mime(&mut self, id: ObjectId, mime: String) {
        self.offers
            .get_mut(&id)
            .expect("announced offer")
            .push(mime);
    }

    fn selection(&mut self, target: SelectionTarget, offer: Option<Offer>) {
        self.selection_events.push((target, offer.is_some()));
        match target {
            SelectionTarget::Clipboard => self.clipboard = offer,
            SelectionTarget::Primary => self.primary = offer,
        }
    }
}

pub struct Peer {
    connection: Connection,
    queue: EventQueue<Events>,
    events: Events,
    device: Device,
    surface: wl_surface::WlSurface,
    sources: Vec<Source>,
    pub(super) server_client: ServerClient,
}

impl Peer {
    pub fn new(server: &Server, protocol: Protocol, access: Arc<Access>) -> Self {
        let (stream, server_client) = server.connect(access);
        let connection = Connection::from_socket(stream).unwrap();
        let mut queue = connection.new_event_queue();
        let qh = queue.handle();
        let registry = connection.display().get_registry(&qh, ());
        let mut events = Events::default();
        roundtrip(&connection, &mut queue, &mut events);
        let bind = |name: &str, maximum: u32| {
            let (id, version) = events.globals[name];
            (id, version.min(maximum))
        };
        let (id, version) = bind("wl_compositor", 6);
        let compositor = registry.bind::<wl_compositor::WlCompositor, _, _>(id, version, &qh, ());
        let surface = compositor.create_surface(&qh, ());
        let (id, version) = bind("wl_seat", 7);
        let seat = registry.bind::<wl_seat::WlSeat, _, _>(id, version, &qh, ());
        seat.get_keyboard(&qh, ());
        seat.get_pointer(&qh, ());
        let device = match protocol {
            Protocol::Core => {
                let (id, version) = bind("wl_data_device_manager", 3);
                let manager = registry.bind::<wl_data_device_manager::WlDataDeviceManager, _, _>(
                    id,
                    version,
                    &qh,
                    (),
                );
                let device = manager.get_data_device(&seat, &qh, ());
                Device::Core(manager, device)
            }
            Protocol::Primary => {
                let (id, version) = bind("zwp_primary_selection_device_manager_v1", 1);
                let manager = registry
                    .bind::<primary_manager::ZwpPrimarySelectionDeviceManagerV1, _, _>(
                        id,
                        version,
                        &qh,
                        (),
                    );
                let device = manager.get_device(&seat, &qh, ());
                Device::Primary(manager, device)
            }
            Protocol::Wlr => {
                let (id, version) = bind("zwlr_data_control_manager_v1", 2);
                let manager = registry.bind::<wlr_manager::ZwlrDataControlManagerV1, _, _>(
                    id,
                    version,
                    &qh,
                    (),
                );
                let device = manager.get_data_device(&seat, &qh, ());
                Device::Wlr(manager, device)
            }
            Protocol::Ext => {
                let (id, version) = bind("ext_data_control_manager_v1", 1);
                let manager = registry.bind::<ext_manager::ExtDataControlManagerV1, _, _>(
                    id,
                    version,
                    &qh,
                    (),
                );
                let device = manager.get_data_device(&seat, &qh, ());
                Device::Ext(manager, device)
            }
        };
        let mut peer = Self {
            connection,
            queue,
            events,
            device,
            surface,
            sources: Vec::new(),
            server_client,
        };
        peer.roundtrip();
        peer
    }

    pub fn roundtrip(&mut self) {
        roundtrip(&self.connection, &mut self.queue, &mut self.events);
    }

    pub(super) fn surface_id(&self) -> u32 {
        self.surface.id().protocol_id()
    }

    pub fn publish(&mut self, server: &Server, target: SelectionTarget, payload: &[u8]) {
        if matches!(self.device, Device::Core(..) | Device::Primary(..)) {
            server.focus(self);
        }
        let qh = self.queue.handle();
        let data = SourcePayload(payload.to_vec());
        let source = match &self.device {
            Device::Core(manager, device) => {
                assert_eq!(target, SelectionTarget::Clipboard);
                let source = manager.create_data_source(&qh, data);
                source.offer(MIME.to_string());
                device.set_selection(Some(&source), 1);
                Source::Core(source)
            }
            Device::Primary(manager, device) => {
                assert_eq!(target, SelectionTarget::Primary);
                let source = manager.create_source(&qh, data);
                source.offer(MIME.to_string());
                device.set_selection(Some(&source), 1);
                Source::Primary(source)
            }
            Device::Wlr(manager, device) => {
                let source = manager.create_data_source(&qh, data);
                source.offer(MIME.to_string());
                match target {
                    SelectionTarget::Clipboard => device.set_selection(Some(&source)),
                    SelectionTarget::Primary => device.set_primary_selection(Some(&source)),
                }
                Source::Wlr(source)
            }
            Device::Ext(manager, device) => {
                let source = manager.create_data_source(&qh, data);
                source.offer(MIME.to_string());
                match target {
                    SelectionTarget::Clipboard => device.set_selection(Some(&source)),
                    SelectionTarget::Primary => device.set_primary_selection(Some(&source)),
                }
                Source::Ext(source)
            }
        };
        assert_ne!(source.id().protocol_id(), 0);
        self.sources.push(source);
        self.roundtrip();
    }

    pub fn clear_selection(&mut self, server: &Server, target: SelectionTarget) {
        if matches!(self.device, Device::Core(..) | Device::Primary(..)) {
            server.focus(self);
        }
        match &self.device {
            Device::Core(_, device) => {
                assert_eq!(target, SelectionTarget::Clipboard);
                device.set_selection(None, 1);
            }
            Device::Primary(_, device) => {
                assert_eq!(target, SelectionTarget::Primary);
                device.set_selection(None, 1);
            }
            Device::Wlr(_, device) => match target {
                SelectionTarget::Clipboard => device.set_selection(None),
                SelectionTarget::Primary => device.set_primary_selection(None),
            },
            Device::Ext(_, device) => match target {
                SelectionTarget::Clipboard => device.set_selection(None),
                SelectionTarget::Primary => device.set_primary_selection(None),
            },
        }
        self.roundtrip();
    }

    pub fn take_selection_events(&mut self) -> Vec<(SelectionTarget, bool)> {
        std::mem::take(&mut self.events.selection_events)
    }

    pub fn offer(&self, target: SelectionTarget) -> Offer {
        match target {
            SelectionTarget::Clipboard => &self.events.clipboard,
            SelectionTarget::Primary => &self.events.primary,
        }
        .clone()
        .expect("expected selection offer")
    }

    pub fn has_offer(&self, target: SelectionTarget) -> bool {
        match target {
            SelectionTarget::Clipboard => self.events.clipboard.is_some(),
            SelectionTarget::Primary => self.events.primary.is_some(),
        }
    }

    pub fn offer_count(&self) -> usize {
        self.events.offers.len()
    }

    pub fn mime_count(&self) -> usize {
        self.events.offers.values().map(Vec::len).sum()
    }

    pub fn source_sends(&self) -> usize {
        self.events.source_sends
    }

    pub fn receive(&mut self, offer: &Offer, mime: &str) -> OwnedFd {
        let (reader, writer) = pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK).unwrap();
        offer.receive(mime, &writer);
        drop(writer);
        self.roundtrip();
        reader
    }

    pub fn start_drag(&mut self, server: &Server, payload: &[u8]) {
        let serial = server.press_pointer(self);
        self.roundtrip();
        let Device::Core(manager, device) = &self.device else {
            panic!("DnD uses the core data device");
        };
        let source =
            manager.create_data_source(&self.queue.handle(), SourcePayload(payload.to_vec()));
        source.offer(MIME.to_string());
        source.set_actions(wl_data_device_manager::DndAction::Copy);
        device.start_drag(Some(&source), &self.surface, None, serial);
        self.sources.push(Source::Core(source));
        self.roundtrip();
    }

    pub fn dnd_offer(&mut self) -> Offer {
        let (offer, serial) = self.events.dnd.clone().expect("DnD enter with offer");
        offer.accept(serial, Some(MIME.to_string()));
        offer.set_actions(
            wl_data_device_manager::DndAction::Copy,
            wl_data_device_manager::DndAction::Copy,
        );
        self.roundtrip();
        Offer::Core(offer)
    }
}

pub fn read_pipe(fd: OwnedFd) -> Vec<u8> {
    let mut payload = Vec::new();
    File::from(fd).take(4096).read_to_end(&mut payload).unwrap();
    payload
}

fn roundtrip(connection: &Connection, queue: &mut EventQueue<Events>, events: &mut Events) {
    events.synced = false;
    connection.display().sync(&queue.handle(), ());
    let deadline = Instant::now() + DEADLINE;
    loop {
        queue.dispatch_pending(events).unwrap();
        connection.flush().unwrap();
        if events.synced {
            break;
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("Wayland roundtrip deadline");
        let Some(guard) = connection.prepare_read() else {
            continue;
        };
        let mut fds = [PollFd::new(connection, PollFlags::IN)];
        let timeout = Timespec::try_from(remaining).unwrap();
        assert_ne!(
            poll(&mut fds, Some(&timeout)).unwrap(),
            0,
            "Wayland socket deadline"
        );
        guard.read().unwrap();
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Events {
    fn event(
        state: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            state.globals.insert(interface, (name, version));
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for Events {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.synced = true;
    }
}

impl Dispatch<wl_data_device::WlDataDevice, ()> for Events {
    fn event(
        state: &mut Self,
        _: &wl_data_device::WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::DataOffer { id } => state.add_offer(Offer::Core(id)),
            wl_data_device::Event::Selection { id } => {
                state.selection(SelectionTarget::Clipboard, id.map(Offer::Core))
            }
            wl_data_device::Event::Enter {
                serial,
                id: Some(id),
                ..
            } => {
                state.dnd = Some((id, serial));
            }
            _ => {}
        }
    }
    event_created_child!(Events, wl_data_device::WlDataDevice, [0 => (wl_data_offer::WlDataOffer, ())]);
}

impl Dispatch<primary_device::ZwpPrimarySelectionDeviceV1, ()> for Events {
    fn event(
        state: &mut Self,
        _: &primary_device::ZwpPrimarySelectionDeviceV1,
        event: primary_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            primary_device::Event::DataOffer { offer } => state.add_offer(Offer::Primary(offer)),
            primary_device::Event::Selection { id } => {
                state.selection(SelectionTarget::Primary, id.map(Offer::Primary))
            }
            _ => {}
        }
    }
    event_created_child!(Events, primary_device::ZwpPrimarySelectionDeviceV1, [0 => (primary_offer::ZwpPrimarySelectionOfferV1, ())]);
}

macro_rules! control_events {
    ($module:ident, $device:ident, $offer_module:ident, $offer:ident, $variant:ident) => {
        impl Dispatch<$module::$device, ()> for Events {
            fn event(
                state: &mut Self,
                _: &$module::$device,
                event: $module::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                match event {
                    $module::Event::DataOffer { id } => state.add_offer(Offer::$variant(id)),
                    $module::Event::Selection { id } => {
                        state.selection(SelectionTarget::Clipboard, id.map(Offer::$variant))
                    }
                    $module::Event::PrimarySelection { id } => {
                        state.selection(SelectionTarget::Primary, id.map(Offer::$variant))
                    }
                    _ => {}
                }
            }
            event_created_child!(Events, $module::$device, [0 => ($offer_module::$offer, ())]);
        }
    };
}

control_events!(
    wlr_device,
    ZwlrDataControlDeviceV1,
    wlr_offer,
    ZwlrDataControlOfferV1,
    Wlr
);
control_events!(
    ext_device,
    ExtDataControlDeviceV1,
    ext_offer,
    ExtDataControlOfferV1,
    Ext
);

macro_rules! offer_events {
    ($module:ident, $offer:ident) => {
        impl Dispatch<$module::$offer, ()> for Events {
            fn event(
                state: &mut Self,
                offer: &$module::$offer,
                event: $module::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                if let $module::Event::Offer { mime_type } = event {
                    state.mime(offer.id(), mime_type);
                }
            }
        }
    };
}

offer_events!(wl_data_offer, WlDataOffer);
offer_events!(primary_offer, ZwpPrimarySelectionOfferV1);
offer_events!(wlr_offer, ZwlrDataControlOfferV1);
offer_events!(ext_offer, ExtDataControlOfferV1);

macro_rules! source_events {
    ($module:ident, $source:ident) => {
        impl Dispatch<$module::$source, SourcePayload> for Events {
            fn event(
                state: &mut Self,
                _: &$module::$source,
                event: $module::Event,
                payload: &SourcePayload,
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                if let $module::Event::Send { mime_type, fd } = event {
                    assert_eq!(mime_type, MIME);
                    state.source_sends += 1;
                    File::from(fd).write_all(&payload.0).unwrap();
                }
            }
        }
    };
}

source_events!(wl_data_source, WlDataSource);
source_events!(primary_source, ZwpPrimarySelectionSourceV1);
source_events!(wlr_source, ZwlrDataControlSourceV1);
source_events!(ext_source, ExtDataControlSourceV1);

delegate_noop!(Events: ignore wl_compositor::WlCompositor);
delegate_noop!(Events: ignore wl_surface::WlSurface);
delegate_noop!(Events: ignore wl_seat::WlSeat);
delegate_noop!(Events: ignore wl_keyboard::WlKeyboard);
delegate_noop!(Events: ignore wl_pointer::WlPointer);
delegate_noop!(Events: ignore wl_data_device_manager::WlDataDeviceManager);
delegate_noop!(Events: ignore primary_manager::ZwpPrimarySelectionDeviceManagerV1);
delegate_noop!(Events: ignore wlr_manager::ZwlrDataControlManagerV1);
delegate_noop!(Events: ignore ext_manager::ExtDataControlManagerV1);
