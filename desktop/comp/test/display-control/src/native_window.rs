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
        shell::wlr_layer::{KeyboardInteractivity, Layer, LayerSurface, LayerSurfaceCachedState},
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

struct Panel {
    surface: Option<LayerSurface>,
    namespace: String,
    layer: Layer,
    configure: Option<Serial>,
    size: Option<(i32, i32)>,
    keyboard_interactivity: Option<String>,
    configured: bool,
    commits: u64,
    raster: Option<serde_json::Value>,
    closed: bool,
    popup: PanelPopup,
}

#[derive(Default)]
struct PanelPopup {
    surface: Option<PopupSurface>,
    geometry: Option<(i32, i32, i32, i32)>,
    configures: Vec<Serial>,
    parent_bound: bool,
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
    panel: Option<Panel>,
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
            panel: None,
        }
    }

    pub fn new_panel(
        &mut self,
        surface: LayerSurface,
        instance: String,
        layer: Layer,
        namespace: String,
    ) -> Result<()> {
        if self.instance.is_some()
            || self.surface.is_some()
            || self.auxiliary.surface.is_some()
            || self.panel.is_some()
            || namespace.len() > 1024
        {
            return Err("only one real same-instance native panel is in scope".into());
        }
        self.instance = Some(instance);
        self.panel = Some(Panel {
            surface: Some(surface),
            namespace,
            layer,
            configure: None,
            size: None,
            keyboard_interactivity: None,
            configured: false,
            commits: 0,
            raster: None,
            closed: false,
            popup: PanelPopup::default(),
        });
        self.publish()
    }

    pub fn ack_panel(&mut self, surface: &WlSurface, serial: Serial) {
        let panel = self
            .panel
            .as_mut()
            .expect("native panel configure without panel");
        assert!(
            panel
                .surface
                .as_ref()
                .is_some_and(|layer| layer.wl_surface() == surface)
        );
        assert_eq!(panel.configure, Some(serial));
        panel.configured = true;
    }

    pub fn close_panel(&mut self, surface: &LayerSurface) -> Result<()> {
        let panel = self
            .panel
            .as_mut()
            .ok_or("native panel destruction without panel")?;
        if panel.surface.as_ref() != Some(surface) {
            return Err("foreign native panel destruction".into());
        }
        panel.surface = None;
        panel.closed = true;
        self.publish()
    }

    pub fn panel_pointer_target(&self, instance: &str) -> Option<(WlSurface, (f64, f64))> {
        if self.instance.as_deref() != Some(instance) {
            return None;
        }
        let panel = self.panel.as_ref()?;
        let surface = panel.surface.as_ref()?.wl_surface();
        let (width, height) = panel.size?;
        let raster = panel.raster.as_ref()?;
        if panel.closed
            || !panel.configured
            || panel.commits == 0
            || !surface.is_alive()
            || panel.keyboard_interactivity.as_deref() != Some("None")
            || raster["nonuniform_pixels"] != true
            || raster["width"].as_i64() != Some(i64::from(width))
            || raster["height"].as_i64() != Some(i64::from(height))
        {
            return None;
        }
        Some((
            surface.clone(),
            (f64::from(width) / 2.0, f64::from(height) / 2.0),
        ))
    }

    fn panel_popup_origin(&self, surface: &WlSurface) -> Result<()> {
        let panel = self.panel.as_ref().ok_or("popup has no native panel")?;
        let parent = panel
            .surface
            .as_ref()
            .ok_or("popup Layer parent is gone")?
            .wl_surface();
        let client = surface.client().ok_or("popup client is gone")?;
        let origin = client
            .get_data::<crate::state::ClientState>()
            .and_then(|state| state.display_origin.as_ref())
            .ok_or("popup has no authenticated display origin")?;
        if !matches!(origin, ClientOrigin::Instance(_))
            || !origin.layer_shell()
            || origin.selection_read()
            || origin.selection_write()
            || origin
                .instance()
                .map(|id| claw_display_control::Epoch(id.0).label())
                != self.instance
            || parent.client().map(|client| client.id()) != Some(client.id())
        {
            return Err("popup is not the live same-client Root panel instance".into());
        }
        Ok(())
    }

    pub fn pending_panel_popup(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
    ) -> Result<()> {
        self.panel_popup_origin(surface.wl_surface())?;
        if self
            .panel_pointer_target(self.instance.as_deref().unwrap())
            .is_none()
        {
            return Err("popup preceded the real configured panel pixels".into());
        }
        let popup = &mut self.panel.as_mut().unwrap().popup;
        if popup.surface.is_some() || popup.closed {
            return Err("only one real Calendar popup lifetime is in scope".into());
        }
        let geometry = positioner.get_geometry();
        if geometry.size.w <= 0
            || geometry.size.h <= 0
            || geometry.size.w > 4096
            || geometry.size.h > 4096
        {
            return Err("native popup geometry exceeded the bounded original request".into());
        }
        surface.with_pending_state(|state| state.geometry = geometry);
        popup.geometry = Some((
            geometry.loc.x,
            geometry.loc.y,
            geometry.size.w,
            geometry.size.h,
        ));
        popup.surface = Some(surface);
        self.publish()
    }

    pub fn bind_panel_popup(&mut self, parent: LayerSurface, surface: PopupSurface) -> Result<()> {
        self.panel_popup_origin(surface.wl_surface())?;
        let panel = self.panel.as_mut().unwrap();
        if panel.surface.as_ref() != Some(&parent)
            || panel.popup.surface.as_ref() != Some(&surface)
            || surface.get_parent_surface().as_ref() != Some(parent.wl_surface())
            || panel.popup.parent_bound
        {
            return Err("popup did not bind exactly the authenticated Layer parent".into());
        }
        panel.popup.parent_bound = true;
        panel.popup.configures.push(surface.send_configure()?);
        self.publish()
    }

    fn reposition_panel_popup(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) -> Result<()> {
        self.panel_popup_origin(surface.wl_surface())?;
        let panel = self.panel.as_mut().unwrap();
        let popup = &mut panel.popup;
        if popup.surface.as_ref() != Some(&surface)
            || !popup.parent_bound
            || popup.closed
            || surface.get_parent_surface().as_ref()
                != panel.surface.as_ref().map(LayerSurface::wl_surface)
            || popup.configures.len() >= 8
        {
            return Err("unbound or excessive native popup reposition".into());
        }
        let geometry = positioner.get_geometry();
        if geometry.size.w <= 0
            || geometry.size.h <= 0
            || geometry.size.w > 4096
            || geometry.size.h > 4096
        {
            return Err("native popup reposition exceeded its bounded original geometry".into());
        }
        surface.with_pending_state(|state| state.geometry = geometry);
        popup.configures.push(surface.send_repositioned(token));
        self.publish()
    }

    fn ack_panel_popup(&mut self, surface: &WlSurface, configure: Configure) {
        let popup = &mut self.panel.as_mut().unwrap().popup;
        assert!(
            popup
                .surface
                .as_ref()
                .is_some_and(|popup| popup.wl_surface() == surface)
        );
        let Configure::Popup(configure) = configure else {
            panic!("native panel popup acknowledged a non-popup configure");
        };
        assert!(popup.parent_bound);
        let issued = popup
            .configures
            .iter()
            .position(|serial| *serial == configure.serial)
            .expect("popup acknowledged a configure not issued to this same Layer child");
        popup.configures.drain(..=issued);
        let geometry = configure.state.geometry;
        popup.geometry = Some((
            geometry.loc.x,
            geometry.loc.y,
            geometry.size.w,
            geometry.size.h,
        ));
        popup.configured = true;
    }

    fn close_panel_popup(&mut self, surface: &PopupSurface) -> Result<()> {
        let popup = &mut self.panel.as_mut().unwrap().popup;
        if popup.surface.as_ref() != Some(surface) {
            return Err("foreign native popup destruction".into());
        }
        popup.surface = None;
        popup.closed = true;
        self.publish()
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
        let is_popup = self.panel.as_ref().is_some_and(|panel| {
            panel
                .popup
                .surface
                .as_ref()
                .is_some_and(|popup| popup.wl_surface() == surface)
        });
        if is_popup {
            self.panel_popup_origin(surface)?;
        }
        let is_panel = self.panel.as_ref().is_some_and(|panel| {
            panel
                .surface
                .as_ref()
                .is_some_and(|layer| layer.wl_surface() == surface)
        });
        if is_panel {
            let client = surface.client().ok_or("native panel client disappeared")?;
            let origin = client
                .get_data::<crate::state::ClientState>()
                .and_then(|state| state.display_origin.as_ref())
                .ok_or("native panel has no authenticated origin")?;
            if !matches!(origin, ClientOrigin::Instance(_))
                || !origin.layer_shell()
                || origin.selection_read()
                || origin.selection_write()
                || origin
                    .instance()
                    .map(|id| claw_display_control::Epoch(id.0).label())
                    != self.instance
            {
                return Err("native panel lost its live Root instance/ui.panel lease".into());
            }
            let panel = self.panel.as_mut().unwrap();
            let current = with_states(surface, |states| {
                *states
                    .cached_state
                    .get::<LayerSurfaceCachedState>()
                    .current()
            });
            if current.layer != panel.layer
                || current.keyboard_interactivity != KeyboardInteractivity::None
            {
                return Err("native panel changed its bounded layer/input role".into());
            }
            panel.keyboard_interactivity = Some(format!("{:?}", current.keyboard_interactivity));
            if panel.configure.is_none() {
                let size = current.size;
                if size.w <= 0 || size.h <= 0 || size.w > 512 || size.h > 128 {
                    return Err(
                        "native panel did not request a bounded original applet size".into(),
                    );
                }
                let layer = panel.surface.as_ref().unwrap();
                layer.with_pending_state(|state| state.size = Some(size));
                panel.size = Some((size.w, size.h));
                panel.configure = Some(layer.send_configure());
            }
        }
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
                    } else if is_panel {
                        let panel = self.panel.as_mut().unwrap();
                        if !panel.configured {
                            return Err(
                                "native panel attached pixels before configure acknowledgement"
                                    .into(),
                            );
                        }
                        panel.raster = Some(raster(&buffer)?);
                        panel.commits = panel
                            .commits
                            .checked_add(1)
                            .ok_or("panel frame count overflow")?;
                        self.output.enter(surface);
                    } else if is_popup {
                        let popup = &mut self.panel.as_mut().unwrap().popup;
                        if !popup.parent_bound || !popup.configured {
                            return Err("native popup attached pixels before parent/configure acknowledgement".into());
                        }
                        popup.raster = Some(raster(&buffer)?);
                        popup.commits = popup
                            .commits
                            .checked_add(1)
                            .ok_or("popup frame count overflow")?;
                        self.output.enter(surface);
                    }
                    buffer.release();
                }
                BufferAssignment::Removed => {
                    if is_top {
                        self.raster = None;
                    } else if is_auxiliary {
                        self.auxiliary.raster = None;
                    } else if is_panel {
                        self.panel.as_mut().unwrap().raster = None;
                    } else if is_popup {
                        self.panel.as_mut().unwrap().popup.raster = None;
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
        if let Some(panel) = &self.panel {
            return self.publish_record(serde_json::json!({
                "schema": "claw.private-native-panel/v1", "instance": instance,
                "role": "zwlr_layer_surface_v1", "layer": format!("{:?}", panel.layer),
                "namespace": panel.namespace, "layer_surfaces": usize::from(panel.surface.is_some()),
                "toplevels": usize::from(self.surface.is_some()) + usize::from(self.auxiliary.surface.is_some()),
                "keyboard_interactivity": panel.keyboard_interactivity,
                "configure_acked": panel.configured, "configure_size": panel.size,
                "buffer_commits": panel.commits, "raster": panel.raster, "closed": panel.closed,
                "popup": {
                    "role": "xdg_popup", "parent_role": "zwlr_layer_surface_v1",
                    "parent_instance": instance, "parent_bound": panel.popup.parent_bound,
                    "open": panel.popup.surface.is_some(), "closed": panel.popup.closed,
                    "geometry": panel.popup.geometry, "configure_acked": panel.popup.configured,
                    "buffer_commits": panel.popup.commits, "raster": panel.popup.raster,
                },
            }));
        }
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
        self.publish_record(record)
    }

    fn publish_record(&mut self, record: serde_json::Value) -> Result<()> {
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

    fn ack_configure(&mut self, surface: WlSurface, configure: Configure) {
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
        } else if window.panel.is_some() {
            window.ack_panel_popup(&surface, configure);
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        if self
            .native_window
            .as_ref()
            .is_some_and(|window| window.panel.is_some())
        {
            self.native_window
                .as_mut()
                .unwrap()
                .pending_panel_popup(surface, positioner)
                .expect("native panel popup admission failed");
            return;
        }
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
        if self
            .native_window
            .as_ref()
            .is_some_and(|window| window.panel.is_some())
        {
            self.native_window
                .as_mut()
                .unwrap()
                .reposition_panel_popup(surface, positioner, token)
                .expect("native Calendar popup reposition failed");
            return;
        }
        surface.with_pending_state(|state| state.geometry = positioner.get_geometry());
        surface.send_repositioned(token);
    }

    fn popup_destroyed(&mut self, surface: PopupSurface) {
        if let Some(window) = self
            .native_window
            .as_mut()
            .filter(|window| window.panel.is_some())
        {
            window
                .close_panel_popup(&surface)
                .expect("native popup closure witness failed");
        }
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
