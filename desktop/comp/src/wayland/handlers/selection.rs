// SPDX-License-Identifier: GPL-3.0-only

use crate::state::{ClientState, State};
use smithay::{
    input::Seat,
    reexports::wayland_server::Client,
    wayland::selection::{SelectionHandler, SelectionSource, SelectionTarget},
    xwayland::xwm::XwmId,
};
use std::os::unix::io::OwnedFd;
use std::sync::Arc;
use tracing::warn;

#[derive(Clone)]
pub enum SelectionData {
    X11(XwmId),
    Root(Arc<claw_display_control::selection::Payload>),
}

impl State {
    pub fn set_root_selection(&mut self, payload: claw_display_control::selection::Payload) {
        let mimes = payload.mime_types();
        let seat = self.common.shell.read().seats.last_active().clone();
        smithay::wayland::selection::data_device::set_data_device_selection(
            &self.common.display_handle, &seat, mimes.clone(), SelectionData::Root(Arc::new(payload)),
        );
        if let Some(id) = self.common.xwayland_state.as_ref()
            .and_then(|state| state.xwm.as_ref()).map(|xwm| xwm.id())
        {
            let focused = self.common.has_x_keyboard_focus(id);
            let state = self.common.xwayland_state.as_mut().unwrap();
            if focused {
                if let Err(error) = state.xwm.as_mut().unwrap()
                    .new_selection(SelectionTarget::Clipboard, Some(mimes))
                { warn!(%error, "Failed to announce OS clipboard to Xwayland"); }
            } else {
                state.clipboard_selection_dirty = Some(mimes);
            }
        }
    }
}

impl SelectionHandler for State {
    type SelectionUserData = SelectionData;

    fn allow_selection_read(client: &Client, _seat: &Seat<Self>, _target: SelectionTarget) -> bool {
        client
            .get_data::<ClientState>()
            .and_then(|data| data.display_origin.as_ref())
            .is_some_and(|origin| origin.selection_read())
    }

    fn allow_selection_write(
        client: &Client,
        _seat: &Seat<Self>,
        _target: SelectionTarget,
    ) -> bool {
        client
            .get_data::<ClientState>()
            .and_then(|data| data.display_origin.as_ref())
            .is_some_and(|origin| origin.selection_write())
    }

    fn new_selection(
        &mut self,
        target: SelectionTarget,
        source: Option<SelectionSource>,
        _seat: Seat<State>,
    ) {
        let Some(xwm_id) = self
            .common
            .xwayland_state
            .as_ref()
            .and_then(|xstate| xstate.xwm.as_ref())
            .map(|xwm| xwm.id())
        else {
            return;
        };

        let x_has_focus = self.common.has_x_keyboard_focus(xwm_id);

        let xstate = self.common.xwayland_state.as_mut().unwrap();
        let xwm = xstate.xwm.as_mut().unwrap();

        if let Some(source) = &source {
            if x_has_focus {
                if let Err(err) = xwm.new_selection(target, Some(source.mime_types())) {
                    warn!(?err, "Failed to set Xwayland clipboard selection.");
                }
            } else {
                match target {
                    SelectionTarget::Clipboard => {
                        xstate.clipboard_selection_dirty = Some(source.mime_types())
                    }
                    SelectionTarget::Primary => {
                        xstate.primary_selection_dirty = Some(source.mime_types())
                    }
                };
            }
        } else {
            if let Err(err) = xwm.new_selection(target, None) {
                warn!(?err, "Failed to clear Xwayland selection.");
            }
            xstate.clipboard_selection_dirty = None;
            xstate.primary_selection_dirty = None;
        }
    }

    fn send_selection(
        &mut self,
        target: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: Seat<State>,
        user_data: &Self::SelectionUserData,
    ) {
        if let SelectionData::Root(payload) = user_data {
            payload.send(&mime_type, fd);
            return;
        }
        let SelectionData::X11(source) = user_data else { return; };
        if let Some(xwm) = self
            .common
            .xwayland_state
            .as_mut()
            .and_then(|xstate| xstate.xwm.as_mut())
            .filter(|xwm| xwm.id() == *source)
            && let Err(err) = xwm.send_selection(target, mime_type, fd)
        {
            warn!(?err, "Failed to send selection (X11 -> Wayland).");
        }
    }
}
