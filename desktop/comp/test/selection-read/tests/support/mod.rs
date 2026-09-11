// SPDX-License-Identifier: GPL-3.0-only

mod client;

pub use client::{Protocol, read_pipe};

use std::{
    fs::File,
    io::Write,
    os::{fd::OwnedFd, unix::net::UnixStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use smithay::{
    backend::input::ButtonState,
    delegate_compositor, delegate_data_control, delegate_data_device, delegate_ext_data_control,
    delegate_primary_selection, delegate_seat,
    input::{
        Seat, SeatHandler, SeatState,
        dnd::{DnDGrab, DndGrabHandler, GrabType, Source},
        pointer::{ButtonEvent, CursorImageStatus, Focus, MotionEvent},
    },
    reexports::wayland_server::{
        Client, Display, DisplayHandle, Resource,
        backend::{ClientData, ClientId, DisconnectReason},
        protocol::wl_surface::WlSurface,
    },
    utils::{SERIAL_COUNTER, Serial},
    wayland::{
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        selection::{
            SelectionHandler, SelectionTarget,
            data_device::{self, DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler},
            ext_data_control,
            primary_selection::{self, PrimarySelectionHandler, PrimarySelectionState},
            wlr_data_control,
        },
    },
};

pub const MIME: &str = "text/plain;charset=utf-8";
pub const DEADLINE: Duration = Duration::from_secs(5);

pub const CASES: &[(Protocol, SelectionTarget)] = &[
    (Protocol::Core, SelectionTarget::Clipboard),
    (Protocol::Primary, SelectionTarget::Primary),
    (Protocol::Wlr, SelectionTarget::Clipboard),
    (Protocol::Wlr, SelectionTarget::Primary),
    (Protocol::Ext, SelectionTarget::Clipboard),
    (Protocol::Ext, SelectionTarget::Primary),
];

pub struct Peer {
    inner: client::Peer,
    server_client: Client,
}

impl std::ops::Deref for Peer {
    type Target = client::Peer;
    fn deref(&self) -> &Self::Target { &self.inner }
}

impl std::ops::DerefMut for Peer {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.inner }
}

impl Peer {
    pub fn new(server: &Server, protocol: Protocol, access: Arc<Access>) -> Self {
        let (stream, server_client) = server.connect(access);
        Self { inner: client::Peer::from_stream(stream, protocol), server_client }
    }

    pub fn publish(&mut self, server: &Server, target: SelectionTarget, payload: &[u8]) {
        if self.requires_focus() { server.focus(self); }
        self.inner.publish_selection(target, payload);
    }

    pub fn clear_selection(&mut self, server: &Server, target: SelectionTarget) {
        if self.requires_focus() { server.focus(self); }
        self.inner.clear_selection(target);
    }

    pub fn start_drag(&mut self, server: &Server, payload: &[u8]) {
        let serial = server.press_pointer(self);
        self.roundtrip();
        self.inner.start_drag_serial(serial, payload);
    }
}

pub struct Access {
    clipboard: AtomicBool,
    primary: AtomicBool,
    checks: Mutex<Vec<SelectionTarget>>,
}

impl Access {
    pub fn new(allowed: bool) -> Arc<Self> {
        Arc::new(Self {
            clipboard: AtomicBool::new(allowed),
            primary: AtomicBool::new(allowed),
            checks: Mutex::new(Vec::new()),
        })
    }

    pub fn set(&self, target: SelectionTarget, allowed: bool) {
        match target {
            SelectionTarget::Clipboard => &self.clipboard,
            SelectionTarget::Primary => &self.primary,
        }
        .store(allowed, Ordering::SeqCst);
    }

    pub fn check_count(&self) -> usize {
        self.checks.lock().unwrap().len()
    }

    fn allows(&self, target: SelectionTarget) -> bool {
        self.checks.lock().unwrap().push(target);
        match target {
            SelectionTarget::Clipboard => &self.clipboard,
            SelectionTarget::Primary => &self.primary,
        }
        .load(Ordering::SeqCst)
    }
}

struct PeerData {
    compositor: CompositorClientState,
    access: Arc<Access>,
}

impl ClientData for PeerData {
    fn initialized(&self, _: ClientId) {}
    fn disconnected(&self, _: ClientId, _: DisconnectReason) {}
}

struct State {
    dh: DisplayHandle,
    compositor: CompositorState,
    seats: SeatState<Self>,
    seat: Seat<Self>,
    data_device: DataDeviceState,
    primary: PrimarySelectionState,
    wlr: wlr_data_control::DataControlState,
    ext: ext_data_control::DataControlState,
    payload_writes: Arc<AtomicUsize>,
    drag_starts: usize,
}

impl State {
    fn new(dh: DisplayHandle, payload_writes: Arc<AtomicUsize>) -> Self {
        let compositor = CompositorState::new::<Self>(&dh);
        let mut seats = SeatState::new();
        let mut seat = seats.new_wl_seat(&dh, "private-selection-fixture");
        seat.add_keyboard(Default::default(), 25, 600).unwrap();
        seat.add_pointer();
        let data_device = DataDeviceState::new::<Self>(&dh);
        let primary = PrimarySelectionState::new::<Self>(&dh);
        let wlr = wlr_data_control::DataControlState::new::<Self, _>(&dh, Some(&primary), |_| true);
        let ext = ext_data_control::DataControlState::new::<Self, _>(&dh, Some(&primary), |_| true);
        Self {
            dh,
            compositor,
            seats,
            seat,
            data_device,
            primary,
            wlr,
            ext,
            payload_writes,
            drag_starts: 0,
        }
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seats
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let client = focused.and_then(|surface| self.dh.get_client(surface.id()).ok());
        data_device::set_data_device_focus(&self.dh, seat, client.clone());
        primary_selection::set_primary_focus(&self.dh, seat, client);
    }

    fn cursor_image(&mut self, _: &Seat<Self>, _: CursorImageStatus) {}
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<PeerData>().unwrap().compositor
    }

    fn commit(&mut self, _: &WlSurface) {}
}

impl SelectionHandler for State {
    type SelectionUserData = Vec<u8>;

    fn allow_selection_read(client: &Client, seat: &Seat<Self>, target: SelectionTarget) -> bool {
        assert_eq!(seat.name(), "private-selection-fixture");
        client.get_data::<PeerData>().unwrap().access.allows(target)
    }

    fn send_selection(
        &mut self,
        _: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _: Seat<Self>,
        payload: &Self::SelectionUserData,
    ) {
        assert_eq!(mime_type, MIME);
        self.payload_writes.fetch_add(1, Ordering::SeqCst);
        File::from(fd).write_all(payload).unwrap();
    }
}

impl DataDeviceHandler for State {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device
    }
}

impl PrimarySelectionHandler for State {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.primary
    }
}

impl wlr_data_control::DataControlHandler for State {
    fn data_control_state(&mut self) -> &mut wlr_data_control::DataControlState {
        &mut self.wlr
    }
}

impl ext_data_control::DataControlHandler for State {
    fn data_control_state(&mut self) -> &mut ext_data_control::DataControlState {
        &mut self.ext
    }
}

impl DndGrabHandler for State {}

impl WaylandDndGrabHandler for State {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        _: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: GrabType,
    ) {
        assert!(matches!(type_, GrabType::Pointer));
        self.drag_starts += 1;
        let pointer = seat.get_pointer().unwrap();
        let start = pointer.grab_start_data().unwrap();
        let grab = DnDGrab::new_pointer(&self.dh, start, source, seat);
        pointer.set_grab(self, grab, serial, Focus::Keep);
    }
}

delegate_compositor!(State);
delegate_seat!(State);
delegate_data_device!(State);
delegate_primary_selection!(State);
delegate_data_control!(State);
delegate_ext_data_control!(State);

enum Command {
    Run(Box<dyn FnOnce(&mut State) + Send>),
    Stop,
}

pub struct Server {
    commands: mpsc::Sender<Command>,
    thread: Option<JoinHandle<()>>,
    payload_writes: Arc<AtomicUsize>,
}

impl Server {
    pub fn new() -> Self {
        let (commands, receiver) = mpsc::channel();
        let payload_writes = Arc::new(AtomicUsize::new(0));
        let writes = payload_writes.clone();
        let thread = thread::spawn(move || {
            let mut display = Display::<State>::new().unwrap();
            let mut state = State::new(display.handle(), writes);
            loop {
                match receiver.recv_timeout(Duration::from_millis(1)) {
                    Ok(Command::Run(run)) => run(&mut state),
                    Ok(Command::Stop) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        panic!("fixture controller disconnected without stopping")
                    }
                }
                display.dispatch_clients(&mut state).unwrap();
                display.flush_clients().unwrap();
            }
        });
        Self {
            commands,
            thread: Some(thread),
            payload_writes,
        }
    }

    fn call<R: Send + 'static>(&self, run: impl FnOnce(&mut State) -> R + Send + 'static) -> R {
        let (tx, rx) = mpsc::sync_channel(1);
        self.commands
            .send(Command::Run(Box::new(move |state| {
                tx.send(run(state)).unwrap();
            })))
            .unwrap();
        rx.recv_timeout(DEADLINE).expect("fixture server deadline")
    }

    fn connect(&self, access: Arc<Access>) -> (UnixStream, Client) {
        let (server, client) = UnixStream::pair().unwrap();
        let peer = self.call(move |state| {
            state
                .dh
                .insert_client(
                    server,
                    Arc::new(PeerData {
                        compositor: CompositorClientState::default(),
                        access,
                    }),
                )
                .unwrap()
        });
        (client, peer)
    }

    pub fn set_selection(&self, target: SelectionTarget, payload: &[u8]) {
        let payload = payload.to_vec();
        self.call(move |state| match target {
            SelectionTarget::Clipboard => data_device::set_data_device_selection(
                &state.dh,
                &state.seat,
                vec![MIME.to_string()],
                payload,
            ),
            SelectionTarget::Primary => primary_selection::set_primary_selection(
                &state.dh,
                &state.seat,
                vec![MIME.to_string()],
                payload,
            ),
        });
    }

    pub fn clear_selection(&self, target: SelectionTarget) {
        self.call(move |state| match target {
            SelectionTarget::Clipboard => {
                data_device::clear_data_device_selection(&state.dh, &state.seat)
            }
            SelectionTarget::Primary => {
                primary_selection::clear_primary_selection(&state.dh, &state.seat)
            }
        });
    }

    pub fn focus(&self, peer: &Peer) {
        let client = peer.server_client.clone();
        let surface_id = peer.surface_id();
        self.call(move |state| {
            let surface = client
                .object_from_protocol_id::<WlSurface>(&state.dh, surface_id)
                .unwrap();
            let keyboard = state.seat.get_keyboard().unwrap();
            keyboard.set_focus(state, Some(surface), SERIAL_COUNTER.next_serial());
        });
    }

    pub fn unfocus(&self) {
        self.call(|state| {
            let keyboard = state.seat.get_keyboard().unwrap();
            keyboard.set_focus(state, None, SERIAL_COUNTER.next_serial());
            // The pinned keyboard's None path does not call focus_changed.
            data_device::set_data_device_focus(&state.dh, &state.seat, None);
            primary_selection::set_primary_focus(&state.dh, &state.seat, None);
        });
    }

    fn press_pointer(&self, peer: &Peer) -> u32 {
        let client = peer.server_client.clone();
        let surface_id = peer.surface_id();
        self.call(move |state| {
            let surface = client
                .object_from_protocol_id::<WlSurface>(&state.dh, surface_id)
                .unwrap();
            let pointer = state.seat.get_pointer().unwrap();
            pointer.motion(
                state,
                Some((surface, (0.0, 0.0).into())),
                &MotionEvent {
                    location: (1.0, 1.0).into(),
                    serial: SERIAL_COUNTER.next_serial(),
                    time: 1,
                },
            );
            let serial = SERIAL_COUNTER.next_serial();
            pointer.button(
                state,
                &ButtonEvent {
                    serial,
                    time: 2,
                    button: 0x110,
                    state: ButtonState::Pressed,
                },
            );
            serial.into()
        })
    }

    pub fn drag_to(&self, peer: &Peer) {
        let client = peer.server_client.clone();
        let surface_id = peer.surface_id();
        self.call(move |state| {
            let surface = client
                .object_from_protocol_id::<WlSurface>(&state.dh, surface_id)
                .unwrap();
            let pointer = state.seat.get_pointer().unwrap();
            pointer.motion(
                state,
                Some((surface, (0.0, 0.0).into())),
                &MotionEvent {
                    location: (2.0, 2.0).into(),
                    serial: SERIAL_COUNTER.next_serial(),
                    time: 3,
                },
            );
        });
    }

    pub fn payload_writes(&self) -> usize {
        self.payload_writes.load(Ordering::SeqCst)
    }

    pub fn drag_starts(&self) -> usize {
        self.call(|state| state.drag_starts)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Err(error) = self.commands.send(Command::Stop) {
            eprintln!("fixture server stop failed: {error}");
        }
        if let Some(thread) = self.thread.take() {
            thread.join().expect("fixture server panicked");
        }
    }
}

struct DefaultHandler(SeatState<Self>);

impl SeatHandler for DefaultHandler {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.0
    }

    fn focus_changed(&mut self, _: &Seat<Self>, _: Option<&WlSurface>) {}
    fn cursor_image(&mut self, _: &Seat<Self>, _: CursorImageStatus) {}
}

impl SelectionHandler for DefaultHandler {
    type SelectionUserData = ();
}

pub fn default_allows(peer: &Peer, target: SelectionTarget) -> bool {
    let mut handler = DefaultHandler(SeatState::new());
    let seat = handler.0.new_seat("default-compatibility");
    DefaultHandler::allow_selection_read(&peer.server_client, &seat, target)
}
