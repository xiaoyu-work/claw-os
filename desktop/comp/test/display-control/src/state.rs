// SPDX-License-Identifier: GPL-3.0-only

use crate::display_authority::{ClientOrigin, DisplayAuthority};
use calloop::LoopHandle;
use smithay::{
    backend::input::ButtonState,
    delegate_compositor, delegate_data_control, delegate_data_device, delegate_ext_data_control,
    delegate_layer_shell, delegate_primary_selection, delegate_seat,
    input::{
        Seat, SeatHandler, SeatState,
        dnd::{DnDGrab, DndGrabHandler, GrabType, Source},
        pointer::{ButtonEvent, CursorImageStatus, Focus, MotionEvent},
    },
    utils::{SERIAL_COUNTER, Serial},
    wayland::{
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        security_context::{SecurityContext, SecurityContextState},
        selection::{
            SelectionHandler, SelectionSource, SelectionTarget,
            data_device::{self, DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler},
            ext_data_control,
            primary_selection::{self, PrimarySelectionHandler, PrimarySelectionState},
            wlr_data_control,
        },
        shell::wlr_layer::{
            Layer, LayerSurface, LayerSurfaceConfigure, WlrLayerShellHandler, WlrLayerShellState,
        },
    },
};
use std::{io::Write, os::fd::OwnedFd};
use wayland_server::{
    Client, DisplayHandle, Resource,
    backend::{ClientData, ClientId, DisconnectReason},
    protocol::{wl_output::WlOutput, wl_surface::WlSurface},
};

#[path = "keyboard.rs"]
mod keyboard;

pub struct ClientState {
    pub compositor: CompositorClientState,
    pub advertised_drm_node: Option<()>,
    #[allow(dead_code)]
    pub security_context: Option<SecurityContext>,
    pub display_origin: Option<ClientOrigin>,
}

impl ClientData for ClientState {
    fn initialized(&self, _: ClientId) {}
    fn disconnected(&self, _: ClientId, _: DisconnectReason) {}
}

pub struct Common {
    pub display_handle: DisplayHandle,
    pub event_loop_handle: LoopHandle<'static, State>,
    pub display_authority: Option<DisplayAuthority>,
    pub should_stop: bool,
}

pub struct State {
    pub common: Common,
    compositor: CompositorState,
    seats: SeatState<Self>,
    seat: Seat<Self>,
    data_device: DataDeviceState,
    primary: PrimarySelectionState,
    wlr: wlr_data_control::DataControlState,
    ext: ext_data_control::DataControlState,
    layer: WlrLayerShellState,
    dragging: bool,
    pub witness: crate::witness::Witness,
    pub native_window: Option<crate::native_window::Window>,
    keyboard_control: keyboard::Control,
}

impl State {
    pub fn new(
        dh: DisplayHandle,
        handle: LoopHandle<'static, Self>,
        authority: DisplayAuthority,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let compositor = CompositorState::new::<Self>(&dh);
        let mut seats = SeatState::new();
        let mut seat = seats.new_wl_seat(&dh, "private-authenticated-display");
        seat.add_keyboard(Default::default(), 25, 600)?;
        seat.add_pointer();
        let data_device = DataDeviceState::new::<Self>(&dh);
        let primary = PrimarySelectionState::new::<Self>(&dh);
        let data_control = |client: &Client| {
            client
                .get_data::<ClientState>()
                .and_then(|data| data.display_origin.as_ref())
                .is_some_and(|origin| origin.selection_read() || origin.selection_write())
        };
        let wlr =
            wlr_data_control::DataControlState::new::<Self, _>(&dh, Some(&primary), data_control);
        let ext =
            ext_data_control::DataControlState::new::<Self, _>(&dh, Some(&primary), data_control);
        let layer = WlrLayerShellState::new_with_filter::<Self, _>(&dh, |client| {
            client
                .get_data::<ClientState>()
                .and_then(|data| data.display_origin.as_ref())
                .is_some_and(ClientOrigin::layer_shell)
        });
        SecurityContextState::new::<Self, _>(&dh, |client| {
            client
                .get_data::<ClientState>()
                .and_then(|data| data.display_origin.as_ref())
                .is_some_and(|origin| origin.can_create_context())
        });
        data_device::set_data_device_selection(
            &dh,
            &seat,
            vec!["text/plain;charset=utf-8".to_string()],
            b"private-clipboard-fixture".to_vec(),
        );
        primary_selection::set_primary_selection(
            &dh,
            &seat,
            vec!["text/plain;charset=utf-8".to_string()],
            b"private-primary-fixture".to_vec(),
        );
        let native_window = crate::native_window::enable(&dh)?;
        Ok(Self {
            common: Common {
                display_handle: dh,
                event_loop_handle: handle,
                display_authority: Some(authority),
                should_stop: false,
            },
            compositor,
            seats,
            seat,
            data_device,
            primary,
            wlr,
            ext,
            layer,
            dragging: false,
            witness: Default::default(),
            native_window,
            keyboard_control: keyboard::Control::default(),
        })
    }

    pub fn new_client_state(&self) -> ClientState {
        ClientState {
            compositor: CompositorClientState::default(),
            advertised_drm_node: None,
            security_context: None,
            display_origin: None,
        }
    }

    pub fn witness_writes(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.witness.dispatch(&self.seat)?;
        let mut keyboard = std::mem::take(&mut self.keyboard_control);
        let delivered = keyboard.step(self);
        self.keyboard_control = keyboard;
        delivered?;
        if let Some(window) = &mut self.native_window {
            window.publish()?;
        }
        Ok(())
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;
    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seats
    }
    fn focus_changed(&mut self, seat: &Seat<Self>, focus: Option<&WlSurface>) {
        let client =
            focus.and_then(|surface| self.common.display_handle.get_client(surface.id()).ok());
        data_device::set_data_device_focus(&self.common.display_handle, seat, client.clone());
        primary_selection::set_primary_focus(&self.common.display_handle, seat, client);
    }
    fn cursor_image(&mut self, _: &Seat<Self>, _: CursorImageStatus) {}
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor
    }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor
    }
    fn commit(&mut self, surface: &WlSurface) {
        if let Some(window) = &mut self.native_window {
            if let Err(error) = window.commit(surface) {
                eprintln!("private actual native window gate: {error}");
                self.common.should_stop = true;
            }
            return;
        }
        self.seat.get_keyboard().unwrap().set_focus(
            self,
            Some(surface.clone()),
            SERIAL_COUNTER.next_serial(),
        );
        let pointer = self.seat.get_pointer().unwrap();
        if !self.dragging {
            // End the previous synthetic press before moving to another client.
            pointer.button(
                self,
                &ButtonEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: 0,
                    button: 0x110,
                    state: ButtonState::Released,
                },
            );
        }
        pointer.motion(
            self,
            Some((surface.clone(), (0.0, 0.0).into())),
            &MotionEvent {
                location: (2.0, 2.0).into(),
                serial: SERIAL_COUNTER.next_serial(),
                time: 1,
            },
        );
        if !self.dragging {
            pointer.button(
                self,
                &ButtonEvent {
                    serial: SERIAL_COUNTER.next_serial(),
                    time: 2,
                    button: 0x110,
                    state: ButtonState::Pressed,
                },
            );
        }
    }
}

impl SelectionHandler for State {
    type SelectionUserData = Vec<u8>;
    fn new_selection(
        &mut self,
        target: SelectionTarget,
        source: Option<SelectionSource>,
        _: Seat<Self>,
    ) {
        self.witness.changed(target, source);
    }
    fn allow_selection_read(client: &Client, _: &Seat<Self>, _: SelectionTarget) -> bool {
        client
            .get_data::<ClientState>()
            .and_then(|data| data.display_origin.as_ref())
            .is_some_and(ClientOrigin::selection_read)
    }
    fn allow_selection_write(client: &Client, _: &Seat<Self>, _: SelectionTarget) -> bool {
        client
            .get_data::<ClientState>()
            .and_then(|data| data.display_origin.as_ref())
            .is_some_and(ClientOrigin::selection_write)
    }
    fn send_selection(
        &mut self,
        _: SelectionTarget,
        _: String,
        fd: OwnedFd,
        _: Seat<Self>,
        bytes: &Vec<u8>,
    ) {
        std::fs::File::from(fd)
            .write_all(bytes)
            .expect("private fixture payload write failed");
    }
}
impl DataDeviceHandler for State {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device
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
        kind: GrabType,
    ) {
        assert!(matches!(kind, GrabType::Pointer));
        self.dragging = true;
        let pointer = seat.get_pointer().unwrap();
        let start = pointer.grab_start_data().unwrap();
        let grab = DnDGrab::new_pointer(&self.common.display_handle, start, source, seat);
        pointer.set_grab(self, grab, serial, Focus::Keep);
    }
}
impl WlrLayerShellHandler for State {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer
    }
    fn new_layer_surface(
        &mut self,
        surface: LayerSurface,
        _: Option<WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        let client = self
            .common
            .display_handle
            .get_client(surface.wl_surface().id())
            .unwrap();
        let origin = client
            .get_data::<ClientState>()
            .unwrap()
            .display_origin
            .as_ref()
            .unwrap();
        assert!(origin.layer_shell());
        assert!(!origin.os_session());
        if let Some(window) = &mut self.native_window {
            assert!(matches!(origin, ClientOrigin::Instance(_)));
            assert!(!origin.selection_read() && !origin.selection_write());
            let instance = claw_display_control::Epoch(origin.instance().unwrap().0).label();
            window
                .new_panel(surface, instance, layer, namespace)
                .expect("native panel admission failed");
            return;
        }
        assert_eq!(layer, Layer::Overlay);
        assert_eq!(namespace, "private-authenticated-panel");
        surface.with_pending_state(|state| state.size = Some((320, 48).into()));
        surface.send_configure();
    }

    fn ack_configure(&mut self, surface: WlSurface, configure: LayerSurfaceConfigure) {
        if let Some(window) = &mut self.native_window {
            window.ack_panel(&surface, configure.serial);
        }
    }

    fn layer_destroyed(&mut self, surface: LayerSurface) {
        if let Some(window) = &mut self.native_window {
            window
                .close_panel(&surface)
                .expect("native panel closure witness failed");
        }
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

delegate_compositor!(State);
delegate_seat!(State);
delegate_data_device!(State);
delegate_primary_selection!(State);
delegate_data_control!(State);
delegate_ext_data_control!(State);
delegate_layer_shell!(State);
