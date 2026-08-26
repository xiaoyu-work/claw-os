//! An animation toolkit for [Iced](https://github.com/iced-rs/iced)
//!
//! > This Project was build for [Cosmic DE](https://github.com/pop-os/cosmic-epoch). Though this will work for any project that depends on [Iced](https://github.com/iced-rs/iced).
//!
//!
//!  The goal of this project is to provide a simple API to build and show
//!  complex animations efficiently in applications built with Iced-rs/Iced.
//!
//! # Project Goals:
//! * Full compatibility with Iced and The Elm Architecture.
//! * Ease of use.
//! * No math required for any animation.
//! * No heap allocations in render loop.
//! * Provide additional animatable widgets.
//! * Custom widget support (create your own!).
//!
//! # Overview
//! To wire cosmic-time into Iced there are five steps to do.
//!
//! 1. Create a [`Timeline`] This is the type that controls the animations.
//! ```ignore
//! struct Counter {
//!       timeline: Timeline
//! }
//!
//! // ~ SNIP
//!
//! impl Application for Counter {
//!     // ~ SNIP
//!      fn new(_flags: ()) -> (Self, Command<Message>) {
//!         (Self { timeline: Timeline::new()}, Command::none())
//!      }
//! }
//! ```
//! 2. Add at least one animation to your timeline. This can be done in your
//!    Application's `new()` or `update()`, or both!
//! ```ignore
//! static CONTAINER: Lazy<id::Container> = Lazy::new(id::Container::unique);
//!
//! let animation = chain![
//!   CONTAINER,
//!   container(Duration::ZERO).width(10),
//!   container(Duration::from_secs(10)).width(100)
//! ];
//! self.timeline.set_chain(animation).start();
//!
//! ```
//! There are some different things here!
//!   > static CONTAINER: Lazy<id::Container> = `Lazy::new(id::Container::unique`);
//!
//!   Cosmic Time refers to each animation with an Id. We export our own, but they are
//!   Identical to the widget Id's Iced uses for widget operations.
//!   Each animatable widget needs an Id. And each Id can only refer to one animation.
//!
//!   > let animation = chain![
//!
//!   Cosmic Time refers to animations as [`Chain`]s because of how we build then.
//!   Each Keyframe is linked together like a chain. The Cosmic Time API doesn't
//!   say "change your width from 10 to 100". We define each state we want the
//!   widget to have `.width(10)` at `Duration::ZERO` then `.width(100)` at
//!   `Duration::from_secs(10)`. Where the `Duration` is the time after the previous
//!   keyframe. This is why we call the animations chains. We cannot get to the
//!   next state without animating though all previous Keyframes.
//!
//!   > `self.timeline.set_chain(animation).start`();
//!
//!   Then we need to add the animation to the [`Timeline`]. We call this `.set_chain`,
//!   because there can only be one chain per Id.
//!   If we `set_chain` with a different animation with the same Id, the first one is
//!   replaced. This a actually a feature not a bug!
//!   As well you can set multiple animations at once:
//!   `self.timeline.set_chain(animation1).set_chain(animation2).start()`
//!
//!   > .start()
//!
//!   This one function call is important enough that we should look at it specifically.
//!   Cosmic Time is atomic, given the animation state held in the [`Timeline`] at any
//!   given time the global animations will be the exact same. The value used to
//!   calculate any animation's interpolation is global. And we use `.start()` to
//!   sync them together.
//!   Say you have two 5 seconds animations running at the same time. They should end
//!   at the same time right? That all depends on when the widget thinks it's animation
//!   should start. `.start()` tells all pending animations to start at the moment that
//!   `.start()` is called. This guarantees they stay in sync.
//!   IMPORTANT! Be sure to only call `.start()` once per call to `update()`.
//!   The below is incorrect!
//!   ```ignore
//!   self.timeline.set_chain(animation1).start();
//!   self.timeline.set_chain(animation2).start();
//!   ```
//!   That code will compile, but will result in the animations not being in sync.
//!
//! 3. Add the Cosmic time Subscription
//! ```ignore
//!   fn subscription(&self) -> Subscription<Message> {
//!        self.timeline.as_subscription::<Event>().map(Message::Tick)
//!    }
//! ```
//!
//! 4. Map the subscription to update the timeline's state:
//! ```ignore
//! fn update(&mut self, message: Message) -> Command<Message> {
//!        match message {
//!            Message::Tick(now) => self.timeline.now(now),
//!        }
//!    }
//! ```
//!   If you skip this step your animations will not progress!
//!
//! 5. Show the widget in your `view()`!
//! ```ignore
//! anim!(CONTIANER, &self.timeline, contents)
//! ```
//!
//! All done!
//! There is a bit of wiring to get Cosmic Time working, but after that it's only
//! a few lines to create rather complex animations!
//! See the Pong example to see how a full game of pong can be implemented in
//! only a few lines!
#![deny(
    missing_debug_implementations,
    missing_docs,
    unused_results,
    clippy::extra_unused_lifetimes,
    clippy::from_over_into,
    clippy::needless_borrow,
    clippy::new_without_default,
    clippy::useless_conversion
)]
#![forbid(unsafe_code, rust_2018_idioms)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::inherent_to_string,
    clippy::type_complexity
)]
#![cfg_attr(docsrs, feature(doc_cfg))]
pub mod reexports;
/// The main timeline for your animations!
pub mod timeline;
/// Additional Widgets that Cosmic Time uses for more advanced animations.
pub mod widget;

mod keyframes;
mod utils;

pub use crate::keyframes::{cards, chain, id, lazy, toggler, Repeat};
pub use crate::timeline::{Chain, Timeline};

pub use cosmic::iced::time::{Duration, Instant};

#[cfg(feature = "once_cell")]
pub use once_cell;

const PI: f32 = std::f32::consts::PI;

/// A simple linear interpolation calculation function.
/// p = `percent_complete` in decimal form
#[must_use]
pub fn lerp(start: f32, end: f32, p: f32) -> f32 {
    (1.0 - p) * start + p * end
}

/// A simple animation percentage flip calculation function.
#[must_use]
pub fn flip(num: f32) -> f32 {
    1.0 - num
}

/// A trait that all ease's need to implement to be used.
pub trait Tween: std::fmt::Debug + Copy {
    /// Takes a linear percentage, and returns tweened value.
    /// p = percent complete as decimal
    fn tween(&self, p: f32) -> f32;
}

/// Speed Controlled Animation use this type.
/// Rather than specifying the time (`Duration`)
/// between links in the animation chain, this
/// type auto-calculates the time for you.
/// Very useful with lazy keyframes.
/// Designed to have an API very similar to `std::time::Duration`
#[derive(Debug, Copy, Clone)]
pub enum Speed {
    /// Whole number of seconds to move per second.
    PerSecond(f32),
    /// Whole number of millisseconds to move per millisecond.
    PerMillis(f32),
    /// Whole number of microseconds to move per microseconds.
    PerMicros(f32),
    /// Whole number of nanoseconds to move per nanosecond.
    PerNanoSe(f32),
}

impl Speed {
    /// Creates a new `Speed` from the specified number of whole seconds.
    #[must_use]
    pub fn per_secs(speed: f32) -> Self {
        Speed::PerSecond(speed)
    }

    /// Creates a new `Speed` from the specified number of whole milliseconds.
    #[must_use]
    pub fn per_millis(speed: f32) -> Self {
        Speed::PerMillis(speed)
    }

    /// Creates a new `Speed` from the specified number of whole microseconds.
    #[must_use]
    pub fn per_micros(speed: f32) -> Self {
        Speed::PerMicros(speed)
    }

    /// Creates a new `Speed` from the specified number of whole nanoseconds.
    #[must_use]
    pub fn per_nanos(speed: f32) -> Self {
        Speed::PerNanoSe(speed)
    }

    fn calc_duration(self, first: f32, second: f32) -> Duration {
        match self {
            Speed::PerSecond(speed) => {
                ((first - second).abs() / speed).round() as u32 * Duration::from_nanos(1e9 as u64)
            }
            Speed::PerMillis(speed) => {
                ((first - second).abs() / speed).round() as u32 * Duration::from_nanos(1e6 as u64)
            }
            Speed::PerMicros(speed) => {
                ((first - second).abs() / speed).round() as u32 * Duration::from_nanos(1000)
            }
            Speed::PerNanoSe(speed) => {
                ((first - second).abs() / speed).round() as u32 * Duration::from_nanos(1)
            }
        }
    }
}

/// A container type so that the API user can specify Either
/// Time controlled animations, or speed controlled animations.
#[derive(Debug, Copy, Clone)]
pub enum MovementType {
    /// Keyframe is time controlled.
    Duration(Duration),
    /// keyframe is speed controlled.
    Speed(Speed),
}

impl From<Duration> for MovementType {
    fn from(duration: Duration) -> Self {
        MovementType::Duration(duration)
    }
}

impl From<Speed> for MovementType {
    fn from(speed: Speed) -> Self {
        MovementType::Speed(speed)
    }
}

macro_rules! tween {
    ($($x:ident),*) => {
        #[derive(Debug, Copy, Clone)]
        /// A container type for all types of animations easings.
        pub enum Ease {
            $(
                /// A container for $x
                $x($x),
            )*
        }

        impl Tween for Ease {
            fn tween(&self, p: f32) -> f32 {
                match self {
                    $(
                        Ease::$x(ease) => ease.tween(p),
                    )*
                }
            }
        }
    };
}

tween!(
    Linear,
    Quadratic,
    Cubic,
    Quartic,
    Quintic,
    Sinusoidal,
    Exponential,
    Circular,
    Elastic,
    Back,
    Bounce
);

/// Used to set a linear animation easing.
/// The default for most animations.
#[derive(Debug, Copy, Clone)]
pub enum Linear {
    /// Modeled after the line y = x
    InOut,
}

impl Tween for Linear {
    fn tween(&self, p: f32) -> f32 {
        p
    }
}

impl From<Linear> for Ease {
    fn from(linear: Linear) -> Self {
        Ease::Linear(linear)
    }
}

/// Used to set a quadratic animation easing.
#[derive(Debug, Copy, Clone)]
pub enum Quadratic {
    /// Modeled after the parabola y = x^2
    In,
    /// Modeled after the parabola y = -x^2 + 2x
    Out,
    /// Modeled after the piecewise quadratic
    /// y = (1/2)((2x)^2)             ; [0, 0.5)
    /// y = -(1/2)((2x-1)*(2x-3) - 1) ; [0.5, 1]
    InOut,
    /// A Bezier Curve TODO
    Bezier(i32),
}

impl Tween for Quadratic {
    fn tween(&self, p: f32) -> f32 {
        match self {
            Quadratic::In => p.powi(2),
            Quadratic::Out => -(p * (p - 2.)),
            Quadratic::InOut => {
                if p < 0.5 {
                    2. * p.powi(2)
                } else {
                    (-2. * p.powi(2)) + p.mul_add(4., -1.)
                }
            }
            Quadratic::Bezier(_n) => p,
        }
    }
}

impl From<Quadratic> for Ease {
    fn from(quadratic: Quadratic) -> Self {
        Ease::Quadratic(quadratic)
    }
}

/// Used to set a cubic animation easing.
#[derive(Debug, Copy, Clone)]
pub enum Cubic {
    /// Modeled after the cubic y = x^3
    In,
    /// Modeled after the cubic y = (x-1)^3 + 1
    Out,
    /// Modeled after the piecewise cubic
    /// y = (1/2)((2x)^3)       ; [0, 0.5]
    /// y = (1/2)((2x-2)^3 + 2) ; [0.5, 1]
    InOut,
}

impl Tween for Cubic {
    fn tween(&self, p: f32) -> f32 {
        match self {
            Cubic::In => p.powi(3),
            Cubic::Out => {
                let q = p - 1.;
                q.powi(3) + 1.
            }
            Cubic::InOut => {
                if p < 0.5 {
                    4. * p.powi(3)
                } else {
                    let q = p.mul_add(2., -2.);
                    (q.powi(3)).mul_add(0.5, 1.)
                }
            }
        }
    }
}

impl From<Cubic> for Ease {
    fn from(cubic: Cubic) -> Self {
        Ease::Cubic(cubic)
    }
}

/// Used to set a quartic animation easing.
#[derive(Debug, Copy, Clone)]
pub enum Quartic {
    /// Modeled after the quartic y = x^4
    In,
    /// Modeled after the quartic y = 1 - (x - 1)^4
    Out,
    /// Modeled after the piecewise quartic
    /// y = (1/2)((2x)^4)       ; [0, 0.5]
    /// y = -(1/2)((2x-2)^4 -2) ; [0.5, 1]
    InOut,
}

impl Tween for Quartic {
    fn tween(&self, p: f32) -> f32 {
        match self {
            Quartic::In => p.powi(4),
            Quartic::Out => {
                let q = p - 1.;
                (q.powi(3)).mul_add(1. - p, 1.)
            }
            Quartic::InOut => {
                if p < 0.5 {
                    8. * p.powi(4)
                } else {
                    let q = p - 1.;
                    (q.powi(4)).mul_add(-8., 1.)
                }
            }
        }
    }
}

impl From<Quartic> for Ease {
    fn from(quartic: Quartic) -> Self {
        Ease::Quartic(quartic)
    }
}

/// Used to set a quintic animation easing.
#[derive(Debug, Copy, Clone)]
pub enum Quintic {
    /// Modeled after the quintic y = x^5
    In,
    /// Modeled after the quintic y = (x - 1)^5 + 1
    Out,
    /// Modeled after the piecewise quintic
    /// y = (1/2)((2x)^5)       ; [0, 0.5]
    /// y = (1/2)((2x-2)^5 + 2) ; [0.5, 1]
    InOut,
}

impl Tween for Quintic {
    fn tween(&self, p: f32) -> f32 {
        match self {
            Quintic::In => p.powi(5),
            Quintic::Out => {
                let q = p - 1.;
                q.powi(5) + 1.
            }
            Quintic::InOut => {
                if p < 0.5 {
                    16. * p.powi(5)
                } else {
                    let q = (2. * p) - 2.;
                    q.powi(5).mul_add(0.5, 1.)
                }
            }
        }
    }
}

impl From<Quintic> for Ease {
    fn from(quintic: Quintic) -> Self {
        Ease::Quintic(quintic)
    }
}

/// Used to set a sinusoildal animation easing.
#[derive(Debug, Copy, Clone)]
pub enum Sinusoidal {
    /// Modeled after eighth sinusoidal wave y = 1 - cos((x * PI) / 2)
    In,
    /// Modeled after eigth sinusoidal wave y = sin((x * PI) / 2)
    Out,
    /// Modeled after quarter sinusoidal wave y = -0.5 * (cos(x * PI) - 1);
    InOut,
}

impl Tween for Sinusoidal {
    fn tween(&self, p: f32) -> f32 {
        match self {
            Sinusoidal::In => 1. - ((p * PI) / 2.).cos(),
            Sinusoidal::Out => ((p * PI) / 2.).sin(),
            Sinusoidal::InOut => -0.5 * ((p * PI).cos() - 1.),
        }
    }
}

impl From<Sinusoidal> for Ease {
    fn from(sinusoidal: Sinusoidal) -> Self {
        Ease::Sinusoidal(sinusoidal)
    }
}

/// Used to set an exponential animation easing.
#[derive(Debug, Copy, Clone)]
pub enum Exponential {
    /// Modeled after the piecewise exponential
    /// y = 0            ; [0, 0]
    /// y = 2^(10x-10)   ; [0, 1]
    In,
    /// Modeled after the piecewise exponential
    /// y = 1 - 2^(-10x)  ; [0, 1]
    /// y = 1             ; [1, 1]
    Out,
    /// Modeled after the piecewise exponential
    /// y = 0                        ; [0, 0  ]
    /// y = 2^(20x - 10) / 2         ; [0, 0.5]
    /// y = 1 - 0.5*2^(-10(2x - 1))  ; [0.5, 1]
    /// y = 1                        ; [1, 1  ]
    InOut,
}

impl Tween for Exponential {
    fn tween(&self, p: f32) -> f32 {
        match self {
            Exponential::In => {
                if p == 0. {
                    0.
                } else {
                    2_f32.powf(10. * p - 10.)
                }
            }
            Exponential::Out => {
                if p == 1. {
                    1.
                } else {
                    1. - 2_f32.powf(-10. * p)
                }
            }
            Exponential::InOut => {
                if p == 0. {
                    0.
                } else if p == 1. {
                    1.
                } else if p < 0.5 {
                    2_f32.powf(p.mul_add(20., -10.)) * 0.5
                } else {
                    2_f32.powf(p.mul_add(-20., 10.)).mul_add(-0.5, 1.)
                }
            }
        }
    }
}

impl From<Exponential> for Ease {
    fn from(exponential: Exponential) -> Self {
        Ease::Exponential(exponential)
    }
}

/// Used to set an circular animation easing.
#[derive(Debug, Copy, Clone)]
pub enum Circular {
    /// Modeled after shifted quadrant IV of unit circle. y = 1 - sqrt(1 - x^2)
    In,
    /// Modeled after shifted quadrant II of unit circle. y = sqrt(1 - (x - 1)^ 2)
    Out,
    /// Modeled after the piecewise circular function
    /// y = (1/2)(1 - sqrt(1 - 2x^2))           ; [0, 0.5)
    /// y = (1/2)(sqrt(1 - ((-2x + 2)^2)) + 1) ; [0.5, 1]
    InOut,
}

impl Tween for Circular {
    fn tween(&self, p: f32) -> f32 {
        match self {
            Circular::In => 1.0 - (1. - (p.powi(2))).sqrt(),
            Circular::Out => ((2. - p) * p).sqrt(),
            Circular::InOut => {
                if p < 0.5 {
                    0.5 * (1. - (1. - (2. * p).powi(2)).sqrt())
                } else {
                    0.5 * ((1. - (-2. * p + 2.).powi(2)).sqrt() + 1.)
                }
            }
        }
    }
}

impl From<Circular> for Ease {
    fn from(circular: Circular) -> Self {
        Ease::Circular(circular)
    }
}

/// Used to set an elastic animation easing.
#[derive(Debug, Copy, Clone)]
pub enum Elastic {
    /// Modeled after damped sin wave: y = sin(13×π/2 x)×2^(10 (x - 1))
    In,
    /// Modeled after damped piecewise sin wave:
    /// y = 2^(-10 x) sin((x×10 - 0.75) (2×π/3)) + 1 [0, 1]
    /// y = 1 [1, 1]
    Out,
    /// Modeled after the piecewise exponentially-damped sine wave:
    /// y = 2^(10 (2 x - 1) - 1) sin(13 π x) [0, 0.5]
    /// y = 1/2 (2 - 2^(-10 (2 x - 1)) sin(13 π x)) [0.5, 1]
    InOut,
}

impl Tween for Elastic {
    fn tween(&self, p: f32) -> f32 {
        match self {
            Elastic::In => (13. * (PI / 2.) * p).sin() * 2_f32.powf(10. * (p - 1.)),
            Elastic::Out => {
                if p == 1. {
                    1.
                } else {
                    2_f32.powf(-10. * p) * ((10. * p - 0.75) * ((2. * PI) / 3.)).sin() + 1.
                }
            }
            Elastic::InOut => {
                if p < 0.5 {
                    2_f32.powf(10. * (2. * p - 1.) - 1.) * (13. * PI * p).sin()
                } else {
                    0.5 * (2. - 2_f32.powf(-20. * p + 10.) * (13. * PI * p).sin())
                }
            }
        }
    }
}

impl From<Elastic> for Ease {
    fn from(elastic: Elastic) -> Self {
        Ease::Elastic(elastic)
    }
}

/// Used to set a back animation easing.
#[derive(Debug, Copy, Clone)]
pub enum Back {
    /// Modeled after the function: y = 2.70158 * x^3 + x^2 * (-1.70158)
    In,
    /// Modeled after the function: y = 1 + 2.70158 (x - 1)^3 + 1.70158 (x - 1)^2
    Out,
    /// Modeled after the piecewise function:
    /// y = (2x)^2 * (1/2 * ((2.5949095 + 1) * 2x - 2.5949095)) [0, 0.5]
    /// y = 1/2 * ((2 x - 2)^2 * ((2.5949095 + 1) * (2x - 2) + 2.5949095) + 2) [0.5, 1]
    InOut,
}

impl Tween for Back {
    fn tween(&self, p: f32) -> f32 {
        match self {
            Back::In => 2.70158 * p.powi(3) - 1.70158 * p.powi(2),
            Back::Out => {
                let q: f32 = p - 1.;
                1. + 2.70158 * q.powi(3) + 1.70158 * q.powi(2)
            }
            Back::InOut => {
                let c = 2.594_909_5;
                if p < 0.5 {
                    let q = 2. * p;
                    q.powi(2) * (0.5 * ((c + 1.) * q - c))
                } else {
                    let q = 2. * p - 2.;
                    0.5 * (q.powi(2) * ((c + 1.) * q + c) + 2.)
                }
            }
        }
    }
}

impl From<Back> for Ease {
    fn from(back: Back) -> Self {
        Ease::Back(back)
    }
}

/// Used to set a bounce animation easing.
#[derive(Debug, Copy, Clone)]
pub enum Bounce {
    /// Bounce before animating in.
    In,
    /// Bounce against end point.
    Out,
    /// Bounce before animating in, then against the end point.
    InOut,
}

impl Bounce {
    fn bounce_ease_in(p: f32) -> f32 {
        1. - Bounce::bounce_ease_out(1. - p)
    }

    fn bounce_ease_out(p: f32) -> f32 {
        if p < 4. / 11. {
            (121. * p.powi(2)) / 16.
        } else if p < 8. / 11. {
            (363. / 40. * p.powi(2)) - 99. / 10. * p + 17. / 5.
        } else if p < 9. / 10. {
            4356. / 361. * p.powi(2) - 35442. / 1805. * p + 16061. / 1805.
        } else {
            54. / 5. * p.powi(2) - 513. / 25. * p + 268. / 25.
        }
    }
}

impl Tween for Bounce {
    fn tween(&self, p: f32) -> f32 {
        match self {
            Bounce::In => Bounce::bounce_ease_in(p),
            Bounce::Out => Bounce::bounce_ease_out(p),
            Bounce::InOut => {
                if p < 0.5 {
                    0.5 * Bounce::bounce_ease_in(p * 2.)
                } else {
                    0.5 + 0.5 * Bounce::bounce_ease_out(p * 2. - 1.)
                }
            }
        }
    }
}

impl From<Bounce> for Ease {
    fn from(bounce: Bounce) -> Self {
        Ease::Bounce(bounce)
    }
}

#[cfg(test)]
#![allow(clippy::excessive_precision)]
mod test {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/test.rs"
    ));
}
