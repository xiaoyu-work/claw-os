//! Client-side binding for `ext_background_effect_v1`.
//!
//! A panel surface is larger than the bar it draws: it also spans the margin
//! to the screen edge, the anchor gap, and the room the autohide animation
//! slides through. Those parts are transparent, but a compositor deciding the
//! blur area from surface geometry has no way to know that, so it frosts them
//! too — which is what put a band of blurred wallpaper alongside the dock.
//!
//! This protocol lets the panel say precisely which rectangle to blur.
//! `layout_` refreshes it from the background element it just placed, so the
//! region follows the bar wherever it moves and whatever size it takes.
//!
//! Neither event carries anything the panel acts on: capabilities are only
//! ever "blur is available", and the panel already only asks for blur.

use sctk::reexports::client::{Connection, Dispatch, QueueHandle};

use cctk::wayland_protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1::{self, ExtBackgroundEffectManagerV1},
    ext_background_effect_surface_v1::{self, ExtBackgroundEffectSurfaceV1},
};

use crate::xdg_shell_wrapper::shared_state::GlobalState;

impl Dispatch<ExtBackgroundEffectManagerV1, ()> for GlobalState {
    fn event(
        _state: &mut Self,
        _manager: &ExtBackgroundEffectManagerV1,
        _event: ext_background_effect_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtBackgroundEffectSurfaceV1, ()> for GlobalState {
    fn event(
        _state: &mut Self,
        _surface: &ExtBackgroundEffectSurfaceV1,
        _event: ext_background_effect_surface_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}
