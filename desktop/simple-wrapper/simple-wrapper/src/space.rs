// SPDX-License-Identifier: MPL-2.0-only

use std::{
    cell::{Cell, RefCell},
    ffi::OsString,
    fs,
    os::unix::{net::UnixStream, prelude::AsRawFd},
    process::Child,
    rc::Rc,
    time::Instant
};
use std::thread::spawn;

use anyhow::bail;
use freedesktop_desktop_entry::{self, DesktopEntry, Iter};
use itertools::Itertools;
use libc::c_int;
use sctk::{
    environment::Environment,
    output::OutputInfo,
    reexports::{
        client::{self, Attached, Main},
        client::protocol::{wl_output as c_wl_output, wl_surface as c_wl_surface},
        protocols::{
            wlr::unstable::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1},
            xdg_shell::client::{
                xdg_popup,
                xdg_positioner::{Anchor, Gravity, XdgPositioner},
                xdg_surface::{self, XdgSurface},
                xdg_wm_base::{XdgWmBase, self},
            },
        },
    },
    shm::AutoMemPool,
};
use slog::{info, Logger, trace};
use smithay::{
    backend::{
        egl::{
            context::{EGLContext, GlAttributes},
            display::EGLDisplay,
            ffi::{
                self,
                egl::{GetConfigAttrib, SwapInterval},
            },
            surface::EGLSurface, self,
        },
        renderer::{
            Bind, Frame, gles2::Gles2Renderer, ImportEgl, Renderer, Unbind,
            utils::draw_surface_tree, ImportDma,
        },
    },
    desktop::{
        draw_window,
        Kind,
        PopupKind,
        PopupManager, space::{RenderError, SurfaceTree}, Space, utils::{damage_from_surface_tree, send_frames_surface_tree, bbox_from_surface_tree}, Window, WindowSurfaceType, draw_popups,
    },
    nix::{fcntl, libc},
    reexports::{wayland_protocols::xdg::shell::server::xdg_toplevel, wayland_server::{
        self, Client, Display as s_Display, DisplayHandle,
        protocol::wl_surface::WlSurface as s_WlSurface,
    }},
    utils::{Logical, Rectangle, Size, Point, Physical},
    wayland::{
        SERIAL_COUNTER,
        shell::xdg::{PopupSurface, PositionerState},
    },
};
use wayland_egl::WlEglSurface;
use xdg_shell_wrapper::{
    client_state::{Env, Focus},
    config::WrapperConfig,
    space::{ClientEglSurface, SpaceEvent, Visibility, WrapperSpace, Popup, PopupState},
    util::{exec_child, get_client_sock},
};

use simple_wrapper_config::SimpleWrapperConfig;

impl Default for SimpleWrapperSpace {
    fn default() -> Self {
        Self {
            popup_manager: PopupManager::new(None),
            full_clear: false,
            config: Default::default(),
            log: Default::default(),
            space: Space::new(None),
            client: Default::default(),
            child: Default::default(),
            last_dirty: Default::default(),
            pending_dimensions: Default::default(),
            next_render_event: Default::default(),
            dimensions: Default::default(),
            focused_surface: Default::default(),
            pool: Default::default(),
            layer_shell: Default::default(),
            output: Default::default(),
            c_display: Default::default(),
            egl_display: Default::default(),
            renderer: Default::default(),
            layer_surface: Default::default(),
            egl_surface: Default::default(),
            layer_shell_wl_surface: Default::default(),
            popups: Default::default(),
            w_accumulated_damage: Default::default(),
        }
    }
}

/// space for the cosmic panel
#[derive(Debug)]
pub struct SimpleWrapperSpace {
    /// config for the panel space
    pub config: SimpleWrapperConfig,
    /// logger for the panel space
    pub log: Option<Logger>,
    pub(crate) space: Space,
    pub(crate) popup_manager: PopupManager,
    pub(crate) client: Option<Client>,
    pub(crate) child: Option<Child>,
    pub(crate) last_dirty: Option<Instant>,
    pub(crate) pending_dimensions: Option<Size<i32, Logical>>,
    pub(crate) full_clear: bool,
    pub(crate) next_render_event: Rc<Cell<Option<SpaceEvent>>>,
    pub(crate) dimensions: Size<i32, Logical>,
    /// focused surface so it can be changed when a window is removed
    focused_surface: Rc<RefCell<Option<s_WlSurface>>>,
    /// visibility state of the panel / panel
    pub(crate) pool: Option<AutoMemPool>,
    pub(crate) layer_shell: Option<Attached<zwlr_layer_shell_v1::ZwlrLayerShellV1>>,
    pub(crate) output: Option<(c_wl_output::WlOutput, OutputInfo)>,
    pub(crate) c_display: Option<client::Display>,
    pub(crate) egl_display: Option<EGLDisplay>,
    pub(crate) renderer: Option<Gles2Renderer>,
    pub(crate) layer_surface: Option<Main<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>>,
    pub(crate) egl_surface: Option<Rc<EGLSurface>>,
    pub(crate) layer_shell_wl_surface: Option<Attached<c_wl_surface::WlSurface>>,
    pub(crate) popups: Vec<Popup>,
    pub(crate) w_accumulated_damage: Vec<Vec<Rectangle<i32, Physical>>>,
}

impl SimpleWrapperSpace {
    /// create a new space for the cosmic panel
    pub fn new(config: SimpleWrapperConfig, log: Logger) -> Self {
        Self {
            config,
            space: Space::new(log.clone()),
            popup_manager: PopupManager::new(log.clone()),
            log: Some(log),
            ..Default::default()
        }
    }

    fn constrain_dim(&self, size: Size<i32, Logical>) -> Size<i32, Logical> {
        let mut w = size.w.try_into().unwrap();
        let mut h = size.h.try_into().unwrap();
        w = 1.max(w);
        h = 1.max(h);
        if let (Some(w_range), _) = self.config.dimensions() {
            if w < w_range.start {
                w = w_range.start;
            } else if w > w_range.end {
                w = w_range.end;
            }
        }
        if let (_, Some(h_range)) = self.config.dimensions() {
            if h < h_range.start {
                h = h_range.start;
            } else if h > h_range.end {
                h = h_range.end;
            }
        }
        (w.try_into().unwrap(), h.try_into().unwrap()).into()
    }

    fn render(&mut self, time: u32) -> Result<(), RenderError<Gles2Renderer>> {
        if self.next_render_event.get() != None {
            return Ok(());
        }
        let clear_color = [0.0, 0.0, 0.0, 0.0];
        let renderer = self.renderer.as_mut().unwrap();
        renderer
            .bind(self.egl_surface.as_ref().unwrap().clone())
            .expect("Failed to bind surface to GL");

        let log_clone = self.log.clone().unwrap();
        if let Some(o) = self.space
            .windows()
            .find_map(|w| self.space.outputs_for_window(w).pop()) {
            let output_size = o.current_mode().ok_or(RenderError::OutputNoMode)?.size;
            // TODO handle fractional scaling?
            // let output_scale = o.current_scale().fractional_scale();
            // We explicitly use ceil for the output geometry size to make sure the damage
            // spans at least the output size. Round and floor would result in parts not drawn as the
            // frame size could be bigger than the maximum the output_geo would define.
            let output_geo = Rectangle::from_loc_and_size(o.current_location(), output_size.to_logical(1));

            let cur_damage = if self.full_clear {
                vec![(Rectangle::from_loc_and_size((0, 0), self.dimensions.to_physical(1)))]
            } else {
                let mut acc_damage = self.space.windows().fold(vec![], |acc, w| {
                    acc.into_iter()
                        .chain(w.accumulated_damage(
                            w.geometry().loc.to_f64().to_physical(1.0),
                            1.0,
                            Some((&self.space, &o)),
                        ))
                        .collect_vec()
                });
                acc_damage.dedup();
                acc_damage.retain(|rect| rect.overlaps(output_geo.to_physical(1)));
                acc_damage.retain(|rect| rect.size.h > 0 && rect.size.w > 0);
                // merge overlapping rectangles
                acc_damage = acc_damage
                    .into_iter()
                    .fold(Vec::new(), |new_damage, mut rect| {
                        // replace with drain_filter, when that becomes stable to reuse the original Vec's memory
                        let (overlapping, mut new_damage): (Vec<_>, Vec<_>) = new_damage
                            .into_iter()
                            .partition(|other| other.overlaps(rect));

                        for overlap in overlapping {
                            rect = rect.merge(overlap);
                        }
                        new_damage.push(rect);
                        new_damage
                    });
                acc_damage
            };

            if let Some(mut damage) = Self::damage_for_buffer(cur_damage, &mut self.w_accumulated_damage, self.egl_surface.as_ref().unwrap()) {
                if damage.is_empty() {
                    damage.push(Rectangle::from_loc_and_size((0, 0), self.dimensions.to_physical(1)));
                }
                let _ = renderer.render(
                    self.dimensions.to_physical(1),
                    smithay::utils::Transform::Flipped180,
                    |renderer: &mut Gles2Renderer, frame| {
                        frame
                            .clear(clear_color, damage.iter().cloned().collect_vec().as_slice())
                            .expect("Failed to clear frame.");
                        for w in self.space.windows() {
                            let mut w_damage = damage.iter().filter_map(|r| r.intersection(w.bbox().to_physical(1))).collect_vec();
                            w_damage.dedup();
                            if damage.len() == 0 {
                                continue;
                            }

                            let _ = draw_window(
                                renderer,
                                frame,
                                w,
                                1.0,
                                w.geometry().loc.to_f64().to_physical(1.0),
                                &w_damage,
                                &log_clone,
                            );
                        }
                    },
                );
                self.egl_surface
                    .as_ref()
                    .unwrap()
                    .swap_buffers(Some(&mut damage))
                    .expect("Failed to swap buffers.");
            }

            // Popup rendering
            for p in self.popups.iter_mut().filter(|p| 
                p.dirty && match p.popup_state.get() {
                    None => true,
                    _ => false,
                }
            ) {
                let _ = renderer.unbind();
                renderer
                .bind(p.egl_surface.clone())
                .expect("Failed to bind surface to GL");
                let p_bbox = bbox_from_surface_tree(p.s_surface.wl_surface(), (0,0));
                let cur_damage = if self.full_clear {
                    vec![p_bbox.to_physical(1)]
                } else {
                    damage_from_surface_tree(p.s_surface.wl_surface(), p_bbox.loc.to_f64().to_physical(1.0), 1.0, Some((&self.space, &o)))
                };

                if let Some(mut damage) = Self::damage_for_buffer(cur_damage, &mut p.accumulated_damage, &p.egl_surface) {
                    if damage.is_empty() {
                        damage.push(p_bbox.to_physical(1));
                    }

                    let _ = renderer.render(
                        p_bbox.size.to_physical(1),
                        smithay::utils::Transform::Flipped180,
                        |renderer: &mut Gles2Renderer, frame| {
                            frame
                                .clear(clear_color, damage.iter().cloned().collect_vec().as_slice())
                                .expect("Failed to clear frame.");

                            let _ = draw_surface_tree(
                                renderer,
                                frame,
                                p.s_surface.wl_surface(),
                                1.0,
                                p_bbox.loc.to_f64().to_physical(1.0),
                                &damage,
                                &log_clone,
                            );
                        },
                    );
                    p.egl_surface
                        .swap_buffers(Some(&mut damage))
                        .expect("Failed to swap buffers.");
                    p.dirty = false;
                }
           }
        }

        self.space.send_frames(time);
        let _ = renderer.unbind();
        self.full_clear = false;
        Ok(())
    }

    fn damage_for_buffer(cur_damage: Vec<Rectangle<i32, Physical>>, acc_damage: &mut Vec<Vec<Rectangle<i32, Physical>>>, egl_surface: &Rc<EGLSurface>) -> Option<Vec<Rectangle<i32, Physical>>> {
        let mut age: usize = egl_surface.buffer_age().unwrap_or_default().try_into().unwrap_or_default();
        let dmg_counts = acc_damage.len();

        let ret = if age == 0 || age > dmg_counts {
            Some(Vec::new())
        } else {
            if !cur_damage.is_empty() {
                acc_damage.push(cur_damage);
                age += 1;
            }
            let mut d = acc_damage.clone();
            d.reverse();
            let d = d[..age+1].into_iter().map(|v| v.into_iter().cloned()).flatten().collect_vec();
            if d.is_empty() {
                // pop front because it won't actually be used
                None
            } else {
                Some(d)
            }
        };

        if acc_damage.len() > 4 {
            acc_damage.drain(..acc_damage.len() - 4);
        }

        // dbg!(age, dmg_counts, &acc_damage, &ret);
        ret
    }
}

impl WrapperSpace for SimpleWrapperSpace {
    type Config = SimpleWrapperConfig;

    fn handle_events(&mut self, dh: &DisplayHandle, time: u32, _: &Focus) -> Instant {
        if self
            .child
            .iter_mut()
            .map(|c| c.try_wait())
            .all(|r| matches!(r, Ok(Some(_))))
        {
            info!(
                self.log.as_ref().unwrap().clone(),
                "Child processes exited. Now exiting..."
            );
            std::process::exit(0);
        }
        let windows = self.space.windows().cloned().collect_vec();
        // for w in &windows {

        //     dbg!(
        //         w.geometry(),
        //         match w.toplevel() {
        //             Kind::Xdg(t) => t.current_state().states.contains(xdg_toplevel::State::Activated),
        //         }
        //     );
        // }
        if !windows.iter().any(|w| {
            match w.toplevel() {
                Kind::Xdg(t) => t.current_state().states.contains(xdg_toplevel::State::Activated),
            }
        }) {
            if let Some(w) = windows.iter().next() {
                self.space.raise_window(w, true);
                self.space.commit(w.toplevel().wl_surface());
                self.space.refresh(dh);
                for w in self.space.windows() {
                    w.configure();
                }
                self.dirty_window(&dh, w.toplevel().wl_surface());
            } else if self.last_dirty.is_some() {
                info!(
                    self.log.as_ref().unwrap().clone(),
                    "Child processes exited. Now exiting..."
                );
                self.child.as_mut().unwrap().kill().unwrap();
                std::process::exit(0);
            }
        }
        let mut should_render = false;
        match self.next_render_event.take() {
            Some(SpaceEvent::Quit) => {
                trace!(
                    self.log.as_ref().unwrap(),
                    "root window removed, exiting..."
                );
                for child in &mut self.child {
                    let _ = child.kill();
                }
            }
            Some(SpaceEvent::Configure {
                     width,
                     height,
                     serial: _serial,
                 }) => {
                if self.dimensions != (width as i32, height as i32).into()
                    && self.pending_dimensions.is_none()
                {
                    self.dimensions = (width as i32, height as i32).into();
                    self.layer_shell_wl_surface.as_ref().unwrap().commit();
                    self.egl_surface
                        .as_ref()
                        .unwrap()
                        .resize(width as i32, height as i32, 0, 0);
                    self.full_clear = true;
                }
            }
            Some(SpaceEvent::WaitConfigure { width, height }) => {
                self.next_render_event
                    .replace(Some(SpaceEvent::WaitConfigure { width, height }));
            }
            None => {
                if let Some(d) = self.pending_dimensions.take() {
                    self.layer_surface
                        .as_ref()
                        .unwrap()
                        .set_size(d.w.try_into().unwrap(), d.h.try_into().unwrap());
                    self.layer_shell_wl_surface.as_ref().unwrap().commit();
                    self.next_render_event
                        .replace(Some(SpaceEvent::WaitConfigure {
                            width: d.w,
                            height: d.h,
                        }));
                } else {
                    should_render = true;
                }
            }
        }

        self.popups.retain_mut(|p: &mut Popup| {
            p.handle_events(&mut self.popup_manager)
        });

        if should_render {
            let _ = self.render(time);
        }
        if self.egl_surface.as_ref().unwrap().get_size() != Some(self.dimensions.to_physical(1)) {
            self.full_clear = true;
        }

        self.last_dirty.unwrap_or_else(|| Instant::now())
    }

    fn popups(&self) -> &[Popup] {
        &self.popups
    }

    fn handle_button(&mut self, c_focused_surface: &c_wl_surface::WlSurface) {
        if self.focused_surface.borrow().is_none()
            && **self.layer_shell_wl_surface.as_ref().unwrap() == *c_focused_surface
        {
            self.close_popups()
        }
    }

    fn add_window(&mut self, w: Window) {
        self.full_clear = true;
        let wl_surface = w.toplevel().wl_surface().clone();
        self.space.map_window(&w, (0, 0), self.z_index().map(|z| z as u8),true);
        self.space.raise_window(&w, true);
        for w in self.space.windows() {
            w.configure();
        }
        self.focused_surface
            .borrow_mut()
            .replace(wl_surface);
    }

    fn add_popup(
        &mut self,
        env: &Environment<Env>,
        xdg_wm_base: &Attached<XdgWmBase>,
        s_surface: PopupSurface,
        positioner: Main<XdgPositioner>,
        PositionerState { rect_size, anchor_rect, anchor_edges, gravity, constraint_adjustment, offset, reactive, parent_size, parent_configure }: PositionerState,
    ) {
        self.close_popups();

        let s = if let Some(s) = self.space.windows().find(|w| {
            match w.toplevel() {
                Kind::Xdg(wl_s) => Some(wl_s.wl_surface()) == s_surface.get_parent_surface().as_ref(),
            }
        }) {
            s
        } else {
            return;
        };

        let c_wl_surface = env.create_surface().detach();
        let c_xdg_surface = xdg_wm_base.get_xdg_surface(&c_wl_surface);

        let wl_surface = s_surface.wl_surface().clone();
        let s_popup_surface = s_surface.clone();
        self.popup_manager.track_popup(PopupKind::Xdg(s_surface.clone())).unwrap();
        self.popup_manager.commit(&wl_surface);

        // dbg!(s.bbox().loc);
        positioner.set_size(rect_size.w, rect_size.h);
        positioner.set_anchor_rect(
            anchor_rect.loc.x + s.bbox().loc.x,
            anchor_rect.loc.y + s.bbox().loc.y,
            anchor_rect.size.w,
            anchor_rect.size.h,
        );
        positioner.set_anchor(Anchor::from_raw(anchor_edges as u32).unwrap_or(Anchor::None));
        positioner.set_gravity(Gravity::from_raw(gravity as u32).unwrap_or(Gravity::None));

        positioner.set_constraint_adjustment(u32::from(constraint_adjustment));
        positioner.set_offset(offset.x, offset.y);
        if positioner.as_ref().version() >= 3 {
            if reactive {
                positioner.set_reactive();
            }
            if let Some(parent_size) = parent_size {
                positioner.set_parent_size(parent_size.w, parent_size.h);
            }
        }
        let c_popup = c_xdg_surface.get_popup(None, &positioner);
        self.layer_surface.as_ref().unwrap().get_popup(&c_popup);

        //must be done after role is assigned as popup
        c_wl_surface.commit();

        let cur_popup_state = Rc::new(Cell::new(Some(PopupState::WaitConfigure)));
        c_xdg_surface.quick_assign(move |c_xdg_surface, e, _| {
            if let xdg_surface::Event::Configure { serial, .. } = e {
                c_xdg_surface.ack_configure(serial);
            }
        });

        let popup_state = cur_popup_state.clone();

        c_popup.quick_assign(move |_c_popup, e, _| {
            if let Some(PopupState::Closed) = popup_state.get().as_ref() {
                return;
            }

            match e {
                xdg_popup::Event::Configure {
                    x,
                    y,
                    width,
                    height,
                } => {
                    if popup_state.get() != Some(PopupState::Closed) {
                        let _ = s_popup_surface.send_configure();
                        popup_state.set(Some(PopupState::Configure {
                            x,
                            y,
                            width,
                            height,
                        }));
                    }
                }
                xdg_popup::Event::PopupDone => {
                    popup_state.set(Some(PopupState::Closed));
                }
                xdg_popup::Event::Repositioned { token } => {
                    popup_state.set(Some(PopupState::Repositioned(token)));
                }
                _ => {}
            };
        });
        let client_egl_surface = ClientEglSurface {
            wl_egl_surface: WlEglSurface::new(&c_wl_surface, rect_size.w, rect_size.h),
            display: self.c_display.as_ref().unwrap().clone(),
        };

        let egl_context = self.renderer.as_ref().unwrap().egl_context();
        let egl_surface = Rc::new(
            EGLSurface::new(
                &self.egl_display.as_ref().unwrap(),
                egl_context
                    .pixel_format()
                    .expect("Failed to get pixel format from EGL context "),
                egl_context.config_id(),
                client_egl_surface,
                self.log.clone(),
            )
            .expect("Failed to initialize EGL Surface"),
        );

        self.popups.push(Popup {
            c_popup,
            c_xdg_surface,
            c_wl_surface: c_wl_surface,
            s_surface,
            egl_surface,
            dirty: false,
            popup_state: cur_popup_state,
            position: (0,0).into(),
            accumulated_damage: Default::default(),
        });
    }

    fn close_popups(&mut self) {
        for w in &mut self.space.windows() {
            for (PopupKind::Xdg(p), _) in
            PopupManager::popups_for_surface(w.toplevel().wl_surface())
            {
                p.send_popup_done();
                self.popup_manager.commit(p.wl_surface());
            }
        }
    }

    ///  update active window based on pointer location
    fn update_pointer(&mut self, (x, y): (i32, i32)) {
        // set new focused
        if let Some((_, s, _)) = self
            .space
            .surface_under((x as f64, y as f64), WindowSurfaceType::ALL)
        {
            self.focused_surface.borrow_mut().replace(s);
            return;
        }
        self.focused_surface.borrow_mut().take();
    }

    fn reposition_popup(
        &mut self,
        s_popup: PopupSurface,
        _: Main<XdgPositioner>,
        _: PositionerState,
        token: u32,
    ) -> anyhow::Result<()> {
        s_popup.send_repositioned(token);
        s_popup.send_configure()?;
        self.popup_manager.commit(s_popup.wl_surface());

        Ok(())
    }

    fn next_space_event(&self) -> Rc<Cell<Option<SpaceEvent>>> {
        Rc::clone(&self.next_render_event)
    }

    fn config(&self) -> Self::Config {
        self.config.clone()
    }

    fn spawn_clients(
        &mut self,
        display: &mut DisplayHandle,
    ) -> Result<Vec<UnixStream>, anyhow::Error> {
        if self.child.is_none() {
            let (client, client_sock) = get_client_sock(display);
            self.client = Some(client);
            // TODO how slow is this? Would it be worth using a faster method of comparing strings?
            self.child = Some(
                Iter::new(freedesktop_desktop_entry::default_paths())
                    .find_map(|path| {
                        if Some(OsString::from(self.config.applet()).as_os_str())
                            == path.file_stem()
                        {
                            fs::read_to_string(&path).ok().and_then(|bytes| {
                                if let Ok(entry) = DesktopEntry::decode(&path, &bytes) {
                                    if let Some(exec) = entry.exec() {
                                        let requests_host_wayland_display =
                                            entry.desktop_entry("HostWaylandDisplay").is_some();
                                        return Some(exec_child(
                                            exec,
                                            Some(self.config.name()),
                                            self.log.as_ref().unwrap().clone(),
                                            client_sock.as_raw_fd(),
                                            requests_host_wayland_display,
                                        ));
                                    }
                                }
                                None
                            })
                        } else {
                            None
                        }
                    })
                    .expect("Failed to spawn client..."),
            );
            Ok(vec![client_sock])
        } else {
            bail!("Clients have already been spawned!");
        }
    }

    fn add_output(
        &mut self,
        output: Option<&c_wl_output::WlOutput>,
        output_info: Option<&OutputInfo>,
        pool: AutoMemPool,
        c_display: client::Display,
        layer_shell: Attached<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
        log: Logger,
        c_surface: Attached<c_wl_surface::WlSurface>,
        focused_surface: Rc<RefCell<Option<s_WlSurface>>>,
    ) -> anyhow::Result<()> {
        if self.layer_shell_wl_surface.is_some()
            || self.output.is_some()
            || self.layer_shell.is_some()
        {
            bail!("output already added!")
        }

        let dimensions = self.constrain_dim((1, 1).into());

        let layer_surface =
            layer_shell.get_layer_surface(&c_surface, output, self.config.layer(), "".to_owned());

        layer_surface.set_anchor(self.config.anchor.into());
        layer_surface.set_keyboard_interactivity(self.config.keyboard_interactivity());
        layer_surface.set_size(
            dimensions.w.try_into().unwrap(),
            dimensions.h.try_into().unwrap(),
        );

        // Commit so that the server will send a configure event
        c_surface.commit();

        let next_render_event = Rc::new(Cell::new(Some(SpaceEvent::WaitConfigure {
            width: dimensions.w,
            height: dimensions.h,
        })));

        //let egl_surface_clone = egl_surface.clone();
        let next_render_event_handle = next_render_event.clone();
        let logger = log.clone();
        layer_surface.quick_assign(move |layer_surface, event, _| {
            match (event, next_render_event_handle.get()) {
                (zwlr_layer_surface_v1::Event::Closed, _) => {
                    info!(logger, "Received close event. closing.");
                    next_render_event_handle.set(Some(SpaceEvent::Quit));
                }
                (
                    zwlr_layer_surface_v1::Event::Configure {
                        serial,
                        width,
                        height,
                    },
                    next,
                ) if next != Some(SpaceEvent::Quit) => {
                    trace!(
                        logger,
                        "received configure event {:?} {:?} {:?}",
                        serial,
                        width,
                        height
                    );
                    layer_surface.ack_configure(serial);
                    next_render_event_handle.set(Some(SpaceEvent::Configure {
                        width: width.try_into().unwrap(),
                        height: height.try_into().unwrap(),
                        serial: serial.try_into().unwrap(),
                    }));
                }
                (_, _) => {}
            }
        });

        let client_egl_surface = ClientEglSurface {
            wl_egl_surface: WlEglSurface::new(&c_surface, dimensions.w, dimensions.h),
            display: c_display.clone(),
        };
        let egl_display = EGLDisplay::new(&client_egl_surface, log.clone())
            .expect("Failed to initialize EGL display");

        let egl_context = EGLContext::new_with_config(
            &egl_display,
            GlAttributes {
                version: (3, 0),
                profile: None,
                debug: cfg!(debug_assertions),
                vsync: false,
            },
            Default::default(),
            log.clone(),
        )
            .expect("Failed to initialize EGL context");

        let mut min_interval_attr = 23239;
        unsafe {
            GetConfigAttrib(
                egl_display.get_display_handle().handle,
                egl_context.config_id(),
                ffi::egl::MIN_SWAP_INTERVAL as c_int,
                &mut min_interval_attr,
            );
        }

        let renderer = unsafe {
            Gles2Renderer::new(egl_context, log.clone()).expect("Failed to initialize EGL Surface")
        };
        trace!(log, "{:?}", unsafe {
            SwapInterval(egl_display.get_display_handle().handle, 0)
        });

        let egl_surface = Rc::new(
            EGLSurface::new(
                &egl_display,
                renderer
                    .egl_context()
                    .pixel_format()
                    .expect("Failed to get pixel format from EGL context "),
                renderer.egl_context().config_id(),
                client_egl_surface,
                log.clone(),
            )
                .expect("Failed to initialize EGL Surface"),
        );

        let next_render_event_handle = next_render_event.clone();
        let logger = log.clone();
        layer_surface.quick_assign(move |layer_surface, event, _| {
            match (event, next_render_event_handle.get()) {
                (zwlr_layer_surface_v1::Event::Closed, _) => {
                    info!(logger, "Received close event. closing.");
                    next_render_event_handle.set(Some(SpaceEvent::Quit));
                }
                (
                    zwlr_layer_surface_v1::Event::Configure {
                        serial,
                        width,
                        height,
                    },
                    next,
                ) if next != Some(SpaceEvent::Quit) => {
                    trace!(
                        logger,
                        "received configure event {:?} {:?} {:?}",
                        serial,
                        width,
                        height
                    );
                    layer_surface.ack_configure(serial);
                    next_render_event_handle.set(Some(SpaceEvent::Configure {
                        width: width.try_into().unwrap(),
                        height: height.try_into().unwrap(),
                        serial: serial.try_into().unwrap(),
                    }));
                }
                (_, _) => {}
            }
        });

        self.output = output.cloned().zip(output_info.cloned());
        self.egl_display.replace(egl_display);
        self.renderer.replace(renderer);
        self.layer_shell.replace(layer_shell);
        self.c_display.replace(c_display);
        self.pool.replace(pool);
        self.layer_surface.replace(layer_surface);
        self.egl_surface.replace(egl_surface);
        self.dimensions = dimensions;
        self.focused_surface = focused_surface;
        self.next_render_event = next_render_event;
        self.full_clear = true;
        self.layer_shell_wl_surface = Some(c_surface);

        Ok(())
    }

    fn log(&self) -> Option<Logger> {
        self.log.clone()
    }

    fn destroy(&mut self) {
        self.layer_surface.as_mut().map(|ls| ls.destroy());
        self.layer_shell_wl_surface
            .as_mut()
            .map(|wls| wls.destroy());
    }

    fn space(&mut self) -> &mut Space {
        &mut self.space
    }

    fn popup_manager(&mut self) -> &mut PopupManager {
        &mut self.popup_manager
    }

    fn visibility(&self) -> Visibility {
        Visibility::Visible
    }

    fn raise_window(&mut self, w: &Window, activate: bool) {
        self.space.raise_window(w, activate);
    }

    fn dirty_window(&mut self, dh: &DisplayHandle, s: &s_WlSurface) {
        self.space.commit(&s);
        self.space.refresh(&dh);
        self.last_dirty = Some(Instant::now());
        if let Some(w) = self.space.window_for_surface(s, WindowSurfaceType::ALL) {
            let size = self.constrain_dim(w.bbox().size);
            let activated = match w.toplevel() {
                Kind::Xdg(t) => t.current_state().states.contains(xdg_toplevel::State::Activated),
            };
            if activated && self.dimensions != size {
                if let Some((_, _)) = &self.output {
                    // TODO improve this for when there are changes to the lists of plugins while running
                    let size = self.constrain_dim(size);
                    let pending_dimensions = self.pending_dimensions.unwrap_or(self.dimensions);
                    let mut wait_configure_dim = self
                        .next_render_event
                        .get()
                        .map(|e| match e {
                            SpaceEvent::Configure {
                                width,
                                height,
                                serial: _serial,
                            } => (width, height),
                            SpaceEvent::WaitConfigure { width, height } => (width, height),
                            _ => self.dimensions.into(),
                        })
                        .unwrap_or(pending_dimensions.into());
                    if self.dimensions.w < size.w
                        && pending_dimensions.w < size.w
                        && wait_configure_dim.0 < size.w
                    {
                        self.pending_dimensions = Some((size.w, wait_configure_dim.1).into());
                        wait_configure_dim.0 = size.w;
                    }
                    if self.dimensions.h < size.h
                        && pending_dimensions.h < size.h
                        && wait_configure_dim.1 < size.h
                    {
                        self.pending_dimensions = Some((wait_configure_dim.0, size.h).into());
                    }
                } else {
                    if self
                        .next_render_event
                        .get()
                        .map(|e| match e {
                            SpaceEvent::Configure {
                                width,
                                height,
                                serial: _serial,
                            } => (width, height).into(),
                            SpaceEvent::WaitConfigure { width, height } => (width, height).into(),
                            _ => self.dimensions,
                        })
                        .unwrap_or(self.pending_dimensions.unwrap_or(self.dimensions))
                        != size
                    {
                        self.pending_dimensions = Some(size);
                        self.full_clear = true;
                    }
                }
            }
            self.space.commit(s);
        }
    }

    fn dirty_popup(&mut self, dh: &DisplayHandle, s: &s_WlSurface) {
        self.space.commit(&s);
        self.space.refresh(&dh);
        self.last_dirty = Some(Instant::now());

        if let Some(p) = self.popups.iter_mut().find(|p| p.s_surface.wl_surface() == s) {
            p.dirty = true;
            self.popup_manager.commit(s);
        }
    }

    fn renderer(&mut self) -> Option<&mut Gles2Renderer> {
        self.renderer.as_mut()
    }
}
