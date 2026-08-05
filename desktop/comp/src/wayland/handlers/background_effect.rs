// SPDX-License-Identifier: GPL-3.0-only
//
// Server-side wiring for `ext_background_effect_v1` (staging Wayland
// protocol). The actual binding code lives in smithay
// (`smithay::wayland::background_effect::*`); we just have to:
//
//   1. Create the global once (done in `State::new` →
//      `BackgroundEffectState::new::<State>(dh)`).
//   2. Implement the [`ExtBackgroundEffectHandler`] trait so the compositor
//      knows which capabilities it advertises (only `Capability::Blur` for
//      now) and gets notified when clients set/unset blur regions.
//   3. Wire the dispatch macros via `delegate_background_effect!(State)`.
//
// Once this is in place every client (libcosmic apps with `is_frosted=true`,
// KDE apps using the protocol, etc.) successfully attaches an
// `ExtBackgroundEffectSurfaceV1` to its `WlSurface` and the blur region is
// available in
// `compositor::with_states(&surface, |s|
//     s.cached_state.get::<BackgroundEffectSurfaceCachedState>().current().blur_region)`
// for the render path to consume.
//
// The render pipeline consumes committed regions for both normal toplevels
// and layer surfaces, placing a dual-Kawase framebuffer effect behind the
// requesting surface.

use smithay::{
    delegate_background_effect,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::{
        background_effect::{Capability, ExtBackgroundEffectHandler},
        compositor::RegionAttributes,
    },
};
use tracing::trace;

use crate::state::State;

impl ExtBackgroundEffectHandler for State {
    fn capabilities(&self) -> Capability {
        // Only `Blur` is defined in v1 of the protocol. If/when the protocol
        // gains additional effects (contrast, brightness, …) we toggle them
        // here once the renderer supports them.
        Capability::Blur
    }

    fn set_blur_region(&mut self, wl_surface: WlSurface, _region: RegionAttributes) {
        // The committed (double-buffered) blur region is reachable via
        // `BackgroundEffectSurfaceCachedState` on the surface's cached
        // state — the renderer reads it during element building, so we
        // don't need to stash anything else here. We do schedule a
        // re-render of every output that currently shows the surface, so
        // a freshly-set region is reflected on the next frame.
        trace!(?wl_surface, "ext_background_effect: set_blur_region");
        self.schedule_render_for_surface(&wl_surface);
    }

    fn unset_blur_region(&mut self, wl_surface: WlSurface) {
        trace!(?wl_surface, "ext_background_effect: unset_blur_region");
        self.schedule_render_for_surface(&wl_surface);
    }
}

delegate_background_effect!(State);

impl State {
    /// Best-effort: damage and reschedule a render on every output that
    /// currently shows the given surface. Used when a blur region changes
    /// so the new effect (or its removal) shows up on the next frame.
    fn schedule_render_for_surface(&mut self, surface: &WlSurface) {
        let outputs: Vec<_> = {
            let shell = self.common.shell.read();
            shell
                .visible_output_for_surface(surface)
                .into_iter()
                .cloned()
                .collect()
        };
        for output in outputs {
            self.backend.schedule_render(&output);
        }
    }
}
