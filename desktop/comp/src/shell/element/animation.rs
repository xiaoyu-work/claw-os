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
// Implementation status:
//
//   * OPEN animation: fully wired up. When a new window is mapped
//     and the config flag is on, we install a [`WindowAnimation`]
//     on the `CosmicMapped`. The render path multiplies its alpha
//     and wraps it in a `RescaleRenderElement` until the spring
//     comes to rest.
//   * CLOSE animation: wired via `Workspace::closing_windows`.
//     `XdgShellHandler::toplevel_destroyed` calls
//     `Shell::begin_close_animation` BEFORE unmapping the surface,
//     which parks a clone of the `CosmicMapped` on the workspace
//     so the WlSurface stays alive (its last committed buffer
//     remains renderable) for the duration of the fade-out spring.
//     `Workspace::update_animations` reaps finished entries.

use std::time::{Duration, Instant};

use crate::backend::render::animations::spring::{Spring, SpringParams};

/// What kind of one-shot animation a window is currently playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAnimationKind {
    Open,
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
        // Opening and closing are different gestures and do not share a
        // curve. A window that is arriving should decelerate into place, and
        // a small overshoot is what reads as landing rather than stopping
        // dead; damping 0.72 peaks about 3.8% past the target around 160ms.
        //
        // Stiffness 800 at our 1.0 mass keeps the whole run near 250ms, fast
        // enough that a quick user does not perceive it as waiting.
        let params = SpringParams::new(0.72, 800.0, 0.0001);
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

    /// Build a CLOSE animation. Played when `xdg_toplevel.destroy`
    /// arrives if `experimental_window_animations` is on. See
    /// `Workspace::closing_windows` for the parking mechanism that
    /// keeps the surface alive while this runs.
    ///
    /// Visuals: scale 1.0 → 0.80 (clearly visible shrink toward the
    /// window centre, à la macOS), alpha 1.0 → 0.0, ~220 ms.
    pub fn close(now: Instant) -> Self {
        // Critically damped, unlike the open curve. A closing window is
        // leaving, so there is nothing for it to settle onto; an overshoot on
        // the way out reads as hesitation.
        let params = SpringParams::new(1.0, 800.0, 0.0001);
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
            to_scale: 0.80,
            from_alpha: 1.0,
            to_alpha: 0.0,
            spring,
        }
    }

    /// Animation progress, clamped to [0.0, 1.0].
    ///
    /// Use this for anything that must stay inside a valid range, such as
    /// alpha. [`Self::overshooting_progress`] keeps the spring's overshoot for
    /// the geometry, where exceeding the target is the whole point.
    pub fn progress(&self, now: Instant) -> f64 {
        self.overshooting_progress(now).clamp(0.0, 1.0)
    }

    /// Progress including the spring's overshoot past 1.0.
    ///
    /// Bounded well above any value an underdamped spring in this range can
    /// reach, purely so a bad config cannot scale a window off the screen.
    pub fn overshooting_progress(&self, now: Instant) -> f64 {
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
        raw.clamp(0.0, 1.25)
    }

    /// Current scale factor to wrap the surface in.
    ///
    /// Sampled with the overshoot intact: the open spring is underdamped, and
    /// clamping here would discard the settle it exists to produce.
    pub fn scale_at(&self, now: Instant) -> f64 {
        let p = self.overshooting_progress(now);
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
