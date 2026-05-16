// SPDX-License-Identifier: GPL-3.0-only
//
// Window open/close spring animations.
//
// Today cosmic-comp pops windows in instantly (and out, on destroy).
// That's the "rough Linux" look the user wants to leave behind. macOS
// and Windows 11 both animate windows on appear/disappear with a
// snappy spring on scale + a fade on alpha.
//
// We re-use the existing spring physics primitive (taken from niri /
// libadwaita; see `backend::render::animations::spring`) so the curve
// matches the workspace-switch motion the rest of the compositor
// already uses — visual consistency between "workspace slides in"
// and "new window pops in" matters a lot for perceived quality.
//
// Gated behind `AppearanceConfig::experimental_window_animations`,
// default `false`.
//
// Implementation status (intentional, documented):
//
//   * OPEN animation: fully wired up. When a new window is mapped
//     and the config flag is on, we install a [`WindowAnimation`]
//     on the `CosmicMapped`. The render path multiplies its alpha
//     and wraps it in a `RescaleRenderElement` until the spring
//     comes to rest.
//   * CLOSE animation: NOT YET WIRED. Reverse-playing a close
//     animation requires holding the surface alive after the client
//     has issued its `xdg_toplevel.destroy` request — Smithay-wise
//     that means a pending-destroy queue on `Shell` plus a tick
//     hook in the render loop. Writing that without a Linux test
//     loop is too easy to get use-after-free. Tracked as a TODO at
//     the `toplevel_destroyed` handler.

use std::time::{Duration, Instant};

use crate::backend::render::animations::spring::{Spring, SpringParams};

/// What kind of one-shot animation a window is currently playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAnimationKind {
    Open,
    /// Reserved — see module docs.
    #[allow(dead_code)]
    Close,
}

/// A scale+alpha animation currently being driven by `Shell::on_commit`
/// and consumed by the renderer.
#[derive(Debug, Clone, Copy)]
pub struct WindowAnimation {
    pub kind: WindowAnimationKind,
    pub started: Instant,
    pub duration: Duration,
    pub from_scale: f64,
    pub to_scale: f64,
    pub from_alpha: f32,
    pub to_alpha: f32,
    pub spring: Spring,
}

impl WindowAnimation {
    /// Build a fresh OPEN animation: scale 0.92 → 1.0, alpha 0.0 →
    /// 1.0, snappy underdamped spring (matches the workspace-switch
    /// spring the rest of the compositor uses).
    pub fn open(now: Instant) -> Self {
        // Damping ratio 0.85 → very mild overshoot, characteristic
        // "snap" of macOS Lion-era window animations. Stiffness 800
        // ≈ 250ms total run time at our 1.0 mass — fast enough that
        // a fast-moving user can't perceive the animation as making
        // them wait.
        let params = SpringParams::new(0.85, 800.0, 0.0001);
        let spring = Spring {
            from: 0.0,
            to: 1.0,
            initial_velocity: 0.0,
            params,
        };
        let duration = spring.duration().min(Duration::from_millis(450));

        Self {
            kind: WindowAnimationKind::Open,
            started: now,
            duration,
            from_scale: 0.92,
            to_scale: 1.0,
            from_alpha: 0.0,
            to_alpha: 1.0,
            spring,
        }
    }

    /// Build a CLOSE animation. Not currently dispatched — see module
    /// docs for the surface-lifetime concern.
    pub fn close(now: Instant) -> Self {
        let params = SpringParams::new(0.85, 800.0, 0.0001);
        let spring = Spring {
            from: 0.0,
            to: 1.0,
            initial_velocity: 0.0,
            params,
        };
        let duration = spring.duration().min(Duration::from_millis(220));

        Self {
            kind: WindowAnimationKind::Close,
            started: now,
            duration,
            from_scale: 1.0,
            to_scale: 0.94,
            from_alpha: 1.0,
            to_alpha: 0.0,
            spring,
        }
    }

    /// Animation progress in [0.0, 1.0]. Reaches 1.0 at `started +
    /// duration`; the caller is expected to clear the animation
    /// (and, for the open case, leave the window at its final
    /// scale+alpha) once `is_done` returns true.
    pub fn progress(&self, now: Instant) -> f64 {
        if now <= self.started {
            return 0.0;
        }
        let elapsed = now - self.started;
        if elapsed >= self.duration {
            return 1.0;
        }
        let t = elapsed.as_secs_f64() / self.duration.as_secs_f64();
        // The spring goes from `from=0` to `to=1`; sample it at the
        // normalised elapsed seconds * spring's own natural duration
        // so the perceived feel stays the same regardless of the
        // capped `duration`.
        let raw = self.spring.value_at(Duration::from_secs_f64(
            t * self.spring.duration().as_secs_f64(),
        ));
        raw.clamp(0.0, 1.0)
    }

    /// Current scale factor to wrap the surface in.
    pub fn scale_at(&self, now: Instant) -> f64 {
        let p = self.progress(now);
        self.from_scale + (self.to_scale - self.from_scale) * p
    }

    /// Current alpha multiplier to apply to the surface.
    pub fn alpha_at(&self, now: Instant) -> f32 {
        let p = self.progress(now) as f32;
        self.from_alpha + (self.to_alpha - self.from_alpha) * p
    }

    /// Has the animation reached its end?
    pub fn is_done(&self, now: Instant) -> bool {
        now.duration_since(self.started) >= self.duration
    }
}
