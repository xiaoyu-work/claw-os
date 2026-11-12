// SPDX-License-Identifier: GPL-3.0-only

use crate::{display_authority::ClientOrigin, state::State};
use smithay::{
    delegate_output, delegate_shm, delegate_xdg_shell,
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    utils::{Serial, Transform},
    wayland::{
        buffer::BufferHandler,
        compositor::{BufferAssignment, SurfaceAttributes, add_pre_commit_hook, with_states},
        output::{OutputHandler, OutputManagerState},
        shell::xdg::{
            Configure, PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler,
            XdgShellState, XdgToplevelSurfaceData,
        },
        shm::{ShmHandler, ShmState, with_buffer_contents},
    },
};
use std::{
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    time::Instant,
};
use wayland_protocols::xdg::shell::server::xdg_toplevel;
use wayland_server::{
    DisplayHandle, Resource,
    protocol::{wl_buffer::WlBuffer, wl_output::WlOutput, wl_seat::WlSeat, wl_surface::WlSurface},
};

pub(crate) const OBSERVER: &str = "/run/user/62050/native-product-observer";
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Default)]
struct Auxiliary {
    surface: Option<ToplevelSurface>,
    configured: bool,
    commits: u64,
    raster: Option<serde_json::Value>,
    closed: bool,
}

#[derive(Default)]
struct FrameTrace {
    top_pre_commits: u64,
    top_pending_buffers: u64,
    top_commits: u64,
    top_current_buffers: u64,
    other_commits: u64,
    other_current_buffers: u64,
    top_frame_callbacks_sent: u64,
    other_frame_callbacks_sent: u64,
    configure_acks: u64,
    output_binds: u64,
}

fn buffer_kind(assignment: Option<&BufferAssignment>) -> u8 {
    match assignment {
        None => 0,
        Some(BufferAssignment::NewBuffer(_)) => 1,
        Some(BufferAssignment::Removed) => 2,
    }
}

pub fn enable(display: &DisplayHandle) -> Result<Option<Window>> {
    let marker = Path::new("/run/gui-public/native-product-enabled");
    match marker.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
        Ok(metadata) => {
            if !metadata.is_file()
                || metadata.uid() != 0
                || metadata.mode() & 0o022 != 0
                || std::fs::read(marker)? != b"private-native-product-v1"
            {
                return Err("unprotected native product fixture selector".into());
            }
            let observer = Path::new(OBSERVER).symlink_metadata()?;
            if !observer.is_dir() || observer.uid() != 62050 || observer.mode() & 0o077 != 0 {
                return Err("unprotected native window observation directory".into());
            }
            Ok(Some(Window::new(display)))
        }
    }
}

pub struct Window {
    shm: ShmState,
    xdg: XdgShellState,
    _outputs: OutputManagerState,
    output: Output,
    surface: Option<ToplevelSurface>,
    instance: Option<String>,
    configured: bool,
    commits: u64,
    raster: Option<serde_json::Value>,
    last: Option<serde_json::Value>,
    started: Instant,
    closed: bool,
    trace: FrameTrace,
    auxiliary: Auxiliary,
}

impl Window {
    pub fn new(display: &DisplayHandle) -> Self {
        let output = Output::new(
            "private-native-product".into(),
            PhysicalProperties {
                size: (210, 158).into(),
                subpixel: Subpixel::Unknown,
                make: "Private acceptance".into(),
                model: "Headless SHM".into(),
                serial_number: "private-native-product".into(),
            },
        );
        output.create_global::<State>(display);
        let mode = Mode {
            size: (800, 600).into(),
            refresh: 60_000,
        };
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            Some(Scale::Integer(1)),
            Some((0, 0).into()),
        );
        output.set_preferred(mode);
        Self {
            shm: ShmState::new::<State>(display, []),
            xdg: XdgShellState::new::<State>(display),
            _outputs: OutputManagerState::new_with_xdg_output::<State>(display),
            output,
            surface: None,
            instance: None,
            configured: false,
            commits: 0,
            raster: None,
            last: None,
            started: Instant::now(),
            closed: false,
            trace: FrameTrace::default(),
            auxiliary: Auxiliary::default(),
        }
    }

    pub fn input_surface(&self, instance: &str, target: &str) -> Option<WlSurface> {
        if self.instance.as_deref() != Some(instance) || self.closed {
            return None;
        }
        let (surface, configured, commits, raster) = match target {
            "primary" if self.auxiliary.surface.is_none() => (
                self.surface.as_ref(),
                self.configured,
                self.commits,
                self.raster.as_ref(),
            ),
            "auxiliary" => (
                self.auxiliary.surface.as_ref(),
                self.auxiliary.configured,
                self.auxiliary.commits,
                self.auxiliary.raster.as_ref(),
            ),
            _ => return None,
        };
        if !configured
            || commits == 0
            || !raster.is_some_and(|pixels| pixels["nonuniform_pixels"] == true)
        {
            return None;
        }
        surface
            .map(ToplevelSurface::wl_surface)
            .filter(|surface| surface.is_alive())
            .cloned()
    }

    fn pre_commit(&mut self, surface: &WlSurface) {
        if self
            .surface
            .as_ref()
            .is_some_and(|top| top.wl_surface() == surface)
        {
            self.trace.top_pre_commits += 1;
            with_states(surface, |states| {
                let mut cached = states.cached_state.get::<SurfaceAttributes>();
                if buffer_kind(cached.pending().buffer.as_ref()) == 1 {
                    self.trace.top_pending_buffers += 1;
                }
            });
        }
    }

    pub fn commit(&mut self, surface: &WlSurface) -> Result<()> {
        let is_top = self
            .surface
            .as_ref()
            .is_some_and(|top| top.wl_surface() == surface);
        let is_auxiliary = self
            .auxiliary
            .surface
            .as_ref()
            .is_some_and(|top| top.wl_surface() == surface);
        let (buffer, callbacks) = with_states(surface, |states| {
            let mut cached = states.cached_state.get::<SurfaceAttributes>();
            let current = cached.current();
            current.damage.clear();
            (
                current.buffer.take(),
                std::mem::take(&mut current.frame_callbacks),
            )
        });
        if is_top {
            self.trace.top_commits += 1;
            self.trace.top_current_buffers += u64::from(buffer_kind(buffer.as_ref()) == 1);
        } else {
            self.trace.other_commits += 1;
            self.trace.other_current_buffers += u64::from(buffer_kind(buffer.as_ref()) == 1);
        }
        if let Some(assignment) = buffer {
            match assignment {
                BufferAssignment::NewBuffer(buffer) => {
                    if is_top {
                        if !self.configured {
                            return Err(
                                "native window attached pixels before configure acknowledgement"
                                    .into(),
                            );
                        }
                        self.raster = Some(raster(&buffer)?);
                        self.commits = self.commits.checked_add(1).ok_or("frame count overflow")?;
                        self.output.enter(surface);
                    } else if is_auxiliary {
                        if !self.auxiliary.configured {
                            return Err(
                                "auxiliary attached pixels before configure acknowledgement".into(),
                            );
                        }
                        self.auxiliary.raster = Some(raster(&buffer)?);
                        self.auxiliary.commits = self
                            .auxiliary
                            .commits
                            .checked_add(1)
                            .ok_or("auxiliary frame count overflow")?;
                        self.output.enter(surface);
                    }
                    buffer.release();
                }
                BufferAssignment::Removed => {
                    if is_top {
                        self.raster = None;
                    } else if is_auxiliary {
                        self.auxiliary.raster = None;
                    }
                }
            }
        }
        if is_top {
            self.trace.top_frame_callbacks_sent += callbacks.len() as u64;
        } else {
            self.trace.other_frame_callbacks_sent += callbacks.len() as u64;
        }
        for callback in callbacks {
            callback.done(self.started.elapsed().as_millis() as u32);
        }
        self.publish()
    }

    pub fn publish(&mut self) -> Result<()> {
        let Some(instance) = &self.instance else {
            return Ok(());
        };
        let (title, app_id) = self.surface.as_ref().map_or((None, None), |surface| {
            with_states(surface.wl_surface(), |states| {
                let role = states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .unwrap()
                    .lock()
                    .unwrap();
                (role.title.clone(), role.app_id.clone())
            })
        });
        let (auxiliary_title, auxiliary_app_id) =
            self.auxiliary
                .surface
                .as_ref()
                .map_or((None, None), |surface| {
                    with_states(surface.wl_surface(), |states| {
                        let role = states
                            .data_map
                            .get::<XdgToplevelSurfaceData>()
                            .unwrap()
                            .lock()
                            .unwrap();
                        (role.title.clone(), role.app_id.clone())
                    })
                });
        let (pending_buffer, pending_callbacks, current_buffer, current_callbacks) =
            self.surface.as_ref().map_or((0, 0, 0, 0), |surface| {
                with_states(surface.wl_surface(), |states| {
                    let mut cached = states.cached_state.get::<SurfaceAttributes>();
                    let pending = cached.pending();
                    let pending_buffer = buffer_kind(pending.buffer.as_ref());
                    let pending_callbacks = pending.frame_callbacks.len();
                    let current = cached.current();
                    (
                        pending_buffer,
                        pending_callbacks,
                        buffer_kind(current.buffer.as_ref()),
                        current.frame_callbacks.len(),
                    )
                })
            });
        if title.as_ref().is_some_and(|value| value.len() > 1024)
            || app_id.as_ref().is_some_and(|value| value.len() > 1024)
            || auxiliary_title
                .as_ref()
                .is_some_and(|value| value.len() > 1024)
            || auxiliary_app_id
                .as_ref()
                .is_some_and(|value| value.len() > 1024)
        {
            return Err("native window metadata exceeded the private witness bound".into());
        }
        let record = serde_json::json!({
            "schema": "claw.private-native-window/v1", "instance": instance,
            "toplevels": usize::from(self.surface.is_some()) + usize::from(self.auxiliary.surface.is_some()),
            "configure_acked": self.configured,
            "buffer_commits": self.commits, "raster": self.raster,
            "title": title, "app_id": app_id, "closed": self.closed,
            "auxiliary": {
                "open": self.auxiliary.surface.is_some(), "closed": self.auxiliary.closed,
                "configure_acked": self.auxiliary.configured, "buffer_commits": self.auxiliary.commits,
                "raster": self.auxiliary.raster, "title": auxiliary_title, "app_id": auxiliary_app_id,
            },
            "render_trace": {
                "top_pre_commits": self.trace.top_pre_commits,
                "top_pending_buffers": self.trace.top_pending_buffers,
                "top_commits": self.trace.top_commits,
                "top_current_buffers": self.trace.top_current_buffers,
                "other_commits": self.trace.other_commits,
                "other_current_buffers": self.trace.other_current_buffers,
                "top_frame_callbacks_sent": self.trace.top_frame_callbacks_sent,
                "other_frame_callbacks_sent": self.trace.other_frame_callbacks_sent,
                "configure_acks": self.trace.configure_acks,
                "output_binds": self.trace.output_binds,
                "pending_buffer_kind": pending_buffer,
                "pending_frame_callbacks": pending_callbacks,
                "current_buffer_kind": current_buffer,
                "current_frame_callbacks": current_callbacks,
            },
        });
        if self.last.as_ref() != Some(&record) {
            let root = Path::new(OBSERVER);
            let candidate = root.join("window.next");
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&candidate)?;
            file.write_all(&serde_json::to_vec(&record)?)?;
            drop(file);
            std::fs::rename(candidate, root.join("window.json"))?;
            self.last = Some(record);
        }
        Ok(())
    }
}

fn raster(buffer: &WlBuffer) -> Result<serde_json::Value> {
    with_buffer_contents(buffer, |pointer, length, metadata| {
        let width = usize::try_from(metadata.width)?;
        let height = usize::try_from(metadata.height)?;
        let stride = usize::try_from(metadata.stride)?;
        let offset = usize::try_from(metadata.offset)?;
        if width == 0 || height == 0 || width > 4096 || height > 4096 || stride < width * 4 {
            return Err("native SHM window has invalid dimensions".into());
        }
        let size = stride
            .checked_mul(height)
            .ok_or("native SHM size overflow")?;
        if size > 64 * 1024 * 1024 || offset.checked_add(size).is_none_or(|end| end > length) {
            return Err("native SHM pixels exceed their actual pool".into());
        }
        // No Rust references may alias this client-writable mapping.
        // The SHM helper also brackets these raw reads with SIGBUS containment.
        let mut first = None;
        let mut nonuniform = false;
        let mut checksum = 0xcbf29ce484222325_u64;
        for row in 0..height {
            for column in 0..width {
                let at = offset + row * stride + column * 4;
                let pixel: [u8; 4] = std::array::from_fn(|index| unsafe {
                    std::ptr::read_volatile(pointer.add(at + index))
                });
                nonuniform |= first.is_some_and(|first| first != pixel);
                first.get_or_insert(pixel);
                for byte in pixel {
                    checksum = (checksum ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
        }
        Ok(serde_json::json!({
            "width": width, "height": height, "nonuniform_pixels": nonuniform,
            "pixel_checksum": format!("{checksum:016x}"),
        }))
    })?
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _: &WlBuffer) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.native_window.as_ref().unwrap().shm
    }
}

impl OutputHandler for State {
    fn output_bound(&mut self, _: Output, _: WlOutput) {
        if let Some(window) = &mut self.native_window {
            window.trace.output_binds += 1;
        }
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.native_window.as_mut().unwrap().xdg
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let client = self
            .common
            .display_handle
            .get_client(surface.wl_surface().id())
            .unwrap();
        let origin = client
            .get_data::<crate::state::ClientState>()
            .unwrap()
            .display_origin
            .as_ref()
            .unwrap();
        assert!(matches!(origin, ClientOrigin::Instance(_)));
        assert!(!origin.layer_shell() && !origin.selection_read() && !origin.selection_write());
        let instance = claw_display_control::Epoch(origin.instance().unwrap().0).label();
        let window = self.native_window.as_mut().unwrap();
        let primary = window.surface.is_none() && !window.closed && window.instance.is_none();
        if primary {
            window.instance = Some(instance);
        } else {
            assert!(
                window.surface.is_some()
                    && !window.closed
                    && window.instance.as_deref() == Some(instance.as_str())
                    && window.auxiliary.surface.is_none()
                    && !window.auxiliary.closed,
                "only one primary and one same-instance auxiliary window are in scope"
            );
        }
        add_pre_commit_hook::<State, _>(surface.wl_surface(), |state, _, surface| {
            if let Some(window) = &mut state.native_window {
                window.pre_commit(surface);
            }
        });
        surface.with_pending_state(|state| {
            state.size = Some((800, 600).into());
            state.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_configure();
        if primary {
            window.surface = Some(surface);
        } else {
            window.auxiliary.surface = Some(surface);
        }
        window
            .publish()
            .expect("native window witness publication failed");
    }

    fn ack_configure(&mut self, surface: WlSurface, _: Configure) {
        let window = self.native_window.as_mut().unwrap();
        if window
            .surface
            .as_ref()
            .is_some_and(|top| top.wl_surface() == &surface)
        {
            window.configured = true;
            window.trace.configure_acks += 1;
        } else if window
            .auxiliary
            .surface
            .as_ref()
            .is_some_and(|top| top.wl_surface() == &surface)
        {
            window.auxiliary.configured = true;
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        surface.with_pending_state(|state| state.geometry = positioner.get_geometry());
        surface
            .send_configure()
            .expect("native popup configure failed");
    }

    fn grab(&mut self, _: PopupSurface, _: WlSeat, _: Serial) {}

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| state.geometry = positioner.get_geometry());
        surface.send_repositioned(token);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let window = self.native_window.as_mut().unwrap();
        if window.surface.as_ref().is_some_and(|top| top == &surface) {
            window.surface = None;
            window.closed = true;
        } else {
            assert!(
                window
                    .auxiliary
                    .surface
                    .as_ref()
                    .is_some_and(|top| top == &surface)
            );
            window.auxiliary.surface = None;
            window.auxiliary.closed = true;
        }
        window
            .publish()
            .expect("native window closure witness failed");
    }
}

delegate_shm!(State);
delegate_output!(State);
delegate_xdg_shell!(State);
