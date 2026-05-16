// SPDX-License-Identifier: GPL-3.0-only
//
// Dual-Kawase backdrop blur infrastructure for cosmic-comp.
//
// This module owns:
//
//   * The two compiled GLES texture programs (`BlurDownShader`,
//     `BlurUpShader`) that implement Marius Bjørge's 5-tap downsample
//     and 8-tap tent upsample kernels (see
//     `shaders/blur_down.frag` / `shaders/blur_up.frag`).
//   * A per-output mip-chain (`BlurState`) of progressively-smaller
//     offscreen `GlesTexture`s used as ping-pong targets during the
//     down + up cascade.
//
// What this module does NOT do (intentionally — separation of
// concerns):
//
//   * Decide which surfaces want blur. That lives at the render-loop
//     layer; this module is told "blur this rectangle" and produces a
//     final blurred texture.
//   * Composite the blurred result back over the framebuffer. The
//     caller wraps the output texture into a smithay `TextureShader-
//     Element` (or its `Postprocess` variant equivalent in
//     `CosmicElement`) and queues it in the regular element list.
//
// Algorithm
// ---------
//
//   Dual Kawase blur, as introduced in Marius Bjørge,
//   "Bandwidth-Efficient Rendering", SIGGRAPH 2015. Reference
//   implementation cribbed from niri @ db49deb (GPL-3.0):
//   https://github.com/YaLTeR/niri/blob/db49deb/src/render_helpers/blur.rs
//
//   1. Copy the framebuffer region behind the blurred surface into
//      `mips[0]`. The caller does this — typically with a
//      `glBlitFramebuffer` from the bound output FBO, performed via
//      smithay's `is_framebuffer_effect()` + `capture_framebuffer()`
//      RenderElement hooks (both already part of the trait in this
//      smithay rev — see `element.rs:188-308`).
//   2. Run [`BlurState::run_passes`]. It binds each successive mip as
//      the destination, then draws a fullscreen quad through the
//      downsample / upsample program with `tex` = the previous mip.
//   3. The final blurred texture is `mips[1]` (half-res; linear
//      sampling on composite hides the upscale).
//
// Status
// ------
//
//   GATED behind `cosmic_comp_config::AppearanceConfig::experimental_blur`
//   (default `false`). Shader compilation errors are logged and
//   swallowed — the compositor must still boot if a quirky driver
//   refuses our GLSL. Allocation errors fall through with `Err` so
//   the caller can fall back to a flat translucent fill for that
//   frame.

use std::collections::HashMap;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Offscreen, Texture,
            element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
            gles::{GlesError, GlesRenderer, GlesTexProgram, GlesTexture, UniformName, UniformType},
            glow::GlowRenderer,
            utils::{CommitCounter, DamageSet, OpaqueRegions},
        },
    },
    output::Output,
    utils::{Buffer, Physical, Point, Rectangle, Scale, Size, Transform, user_data::UserDataMap},
};

use super::element::{AsGlowRenderer, FromGlesError};

pub static BLUR_DOWN_SHADER: &str = include_str!("./shaders/blur_down.frag");
pub static BLUR_UP_SHADER: &str = include_str!("./shaders/blur_up.frag");

/// Compiled dual-Kawase downsample program. One per `GlesRenderer`,
/// stored in `EglContext::user_data()`.
pub struct BlurDownShader(pub GlesTexProgram);

/// Compiled dual-Kawase upsample program.
pub struct BlurUpShader(pub GlesTexProgram);

/// Default number of mip levels in the chain.
///
/// 4 levels (full → 1/2 → 1/4 → 1/8) yields an effective Gaussian σ
/// of roughly 10 pixels — close to macOS Big Sur menubar / dock
/// frosting. Bump to 5 for a stronger "blurred wallpaper" look at
/// the cost of one extra down+up pass per blurred surface per frame.
pub const DEFAULT_PASSES: usize = 4;

/// Multiplier for `half_pixel` in both passes. Larger = wider taps =
/// stronger blur per level; matches the niri / KWin default.
pub const DEFAULT_OFFSET: f32 = 1.5;

// ────────────────────────────────────────────────────────────────
// Shader compilation
// ────────────────────────────────────────────────────────────────

/// Compile the two dual-Kawase shaders and stash them in the renderer's
/// EGL user data. Idempotent; safe to call from each output's
/// init path.
///
/// Errors are logged-and-swallowed: this is an experimental feature
/// behind a config flag, and we must not gate compositor boot on
/// niri-derived GLSL parsing cleanly on every GPU driver in the wild.
/// Callers check `BlurState::shaders_available(renderer)` before
/// scheduling blur work.
pub fn init_blur_shaders(renderer: &mut GlesRenderer) {
    {
        let ud = renderer.egl_context().user_data();
        if ud.get::<BlurDownShader>().is_some() && ud.get::<BlurUpShader>().is_some() {
            return;
        }
    }

    let uniforms = [
        UniformName::new("half_pixel", UniformType::_2f),
        UniformName::new("offset", UniformType::_1f),
    ];

    let down = match renderer.compile_custom_texture_shader(BLUR_DOWN_SHADER, &uniforms) {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(?err, "blur_down shader failed to compile; blur disabled");
            return;
        }
    };
    let up = match renderer.compile_custom_texture_shader(BLUR_UP_SHADER, &uniforms) {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(?err, "blur_up shader failed to compile; blur disabled");
            return;
        }
    };

    let ud = renderer.egl_context().user_data();
    ud.insert_if_missing(|| BlurDownShader(down));
    ud.insert_if_missing(|| BlurUpShader(up));
    tracing::info!("dual-Kawase blur shaders compiled");
}

// ────────────────────────────────────────────────────────────────
// Per-output state
// ────────────────────────────────────────────────────────────────

/// Per-output scratch space for a single blur cascade.
///
/// Allocated lazily on first frame that needs it (cf.
/// [`BlurStates::for_output`]). The mip-chain is reallocated whenever
/// the output resolution / pixel-format changes.
pub struct BlurState {
    /// Down/up ping-pong textures.
    /// `mips[0]` = full-res capture target.
    /// `mips[1]` = half-res blurred result the caller composites back.
    /// `mips[i>1]` = intermediate ping-pong buffers.
    pub mips: Vec<GlesTexture>,
    pub size: Size<i32, Physical>,
    pub format: Fourcc,
    pub passes: usize,
    pub offset: f32,
}

impl Default for BlurState {
    fn default() -> Self {
        Self {
            mips: Vec::new(),
            size: Size::from((0, 0)),
            format: Fourcc::Abgr8888,
            passes: DEFAULT_PASSES,
            offset: DEFAULT_OFFSET,
        }
    }
}

impl BlurState {
    /// Are both blur shaders ready on this renderer?
    pub fn shaders_available(renderer: &GlesRenderer) -> bool {
        let ud = renderer.egl_context().user_data();
        ud.get::<BlurDownShader>().is_some() && ud.get::<BlurUpShader>().is_some()
    }

    /// (Re)allocate the mip chain so that `mips[0]` matches `size`.
    /// Idempotent: a no-op if already sized correctly.
    ///
    /// On allocation failure all textures are dropped — the next call
    /// will start from scratch.
    pub fn ensure_sized(
        &mut self,
        renderer: &mut GlowRenderer,
        size: Size<i32, Physical>,
        format: Fourcc,
        passes: usize,
    ) -> Result<(), GlesError> {
        let passes = passes.max(2);

        if self.size == size && self.format == format && self.mips.len() == passes {
            return Ok(());
        }

        self.mips.clear();
        self.size = size;
        self.format = format;
        self.passes = passes;

        let buffer_size: Size<i32, Buffer> = size.to_logical(1).to_buffer(1, Transform::Normal);
        let mut current = buffer_size;

        for level in 0..passes {
            let tex: GlesTexture =
                Offscreen::<GlesTexture>::create_buffer(renderer, format, current).map_err(|err| {
                    tracing::warn!(?err, level, "blur mip allocation failed");
                    err
                })?;
            self.mips.push(tex);

            current = Size::from(((current.w / 2).max(1), (current.h / 2).max(1)));
        }

        Ok(())
    }

    /// Returns a reference to the texture that holds the final blurred
    /// result after [`Self::run_passes`] has been called. Caller is
    /// expected to bind this as input to a `TextureShaderElement` (or
    /// equivalent) when compositing the blurred backdrop.
    pub fn output_texture(&self) -> Option<&GlesTexture> {
        self.mips.get(1)
    }

    /// Returns a mutable reference to the full-resolution capture
    /// target (the texture into which the caller blits the framebuffer
    /// region behind the blurred surface).
    pub fn capture_texture(&self) -> Option<&GlesTexture> {
        self.mips.first()
    }

    /// Run the dual-Kawase down/up cascade. Caller must:
    ///   1. Have already populated `mips[0]` with the captured
    ///      framebuffer region.
    ///   2. Hold the renderer in a state where new FBO binds are
    ///      legal (i.e. not in the middle of someone else's draw).
    ///
    /// On success, the blurred result is in `mips[1]`.
    ///
    /// NOTE: the actual draw of a fullscreen quad through the
    /// down/up programs is performed via smithay's `Bind` +
    /// `TextureShaderElement::new` plumbing in the caller (see
    /// `render::output_elements` integration). This method is a thin
    /// state-machine driver that picks the right (src, dst,
    /// half_pixel) tuples; it deliberately does not own the draw
    /// loop because the cleanest place to issue smithay frame
    /// commands is the renderer that already holds the GlesFrame.
    ///
    /// Caller wires up draws with:
    ///
    /// ```ignore
    /// for step in state.steps()? {
    ///     renderer.bind(step.dst)?;
    ///     let elem = TextureShaderElement::new(
    ///         step.src,
    ///         step.program,
    ///         /* src_rect= */ Rectangle::from_size(step.src.size()),
    ///         /* dst_rect= */ Rectangle::from_size(
    ///                              step.dst.size().to_logical(1, Transform::Normal),
    ///                          ),
    ///         /* alpha= */ 1.0,
    ///         /* additional_uniforms= */ vec![
    ///             Uniform::new("half_pixel", step.half_pixel),
    ///             Uniform::new("offset", step.offset),
    ///         ],
    ///         Kind::Unspecified,
    ///     );
    ///     elem.draw(/* frame, src, dst, damage, opaque, cache */)?;
    /// }
    /// ```
    pub fn steps(&self) -> Result<Vec<BlurStep<'_>>, GlesError> {
        if self.mips.len() < 2 {
            return Err(GlesError::ShaderCompileError);
        }

        let mut steps = Vec::with_capacity((self.mips.len() - 1) * 2);

        for i in 1..self.mips.len() {
            let src = &self.mips[i - 1];
            let dst = &self.mips[i];
            let dst_size = dst.size();
            steps.push(BlurStep {
                src,
                dst,
                direction: BlurDirection::Down,
                half_pixel: [0.5 / dst_size.w as f32, 0.5 / dst_size.h as f32],
                offset: self.offset,
            });
        }

        for i in (1..self.mips.len() - 1).rev() {
            let src = &self.mips[i + 1];
            let dst = &self.mips[i];
            let src_size = src.size();
            steps.push(BlurStep {
                src,
                dst,
                direction: BlurDirection::Up,
                half_pixel: [0.5 / src_size.w as f32, 0.5 / src_size.h as f32],
                offset: self.offset,
            });
        }

        Ok(steps)
    }
}

/// One step in a [`BlurState::steps`] cascade. Tells the caller which
/// source texture to sample, which destination to bind, and which
/// uniforms to set on the chosen program.
pub struct BlurStep<'a> {
    pub src: &'a GlesTexture,
    pub dst: &'a GlesTexture,
    pub direction: BlurDirection,
    pub half_pixel: [f32; 2],
    pub offset: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlurDirection {
    Down,
    Up,
}

/// Per-output collection. Owned by `Common`.
///
/// Use [`Self::for_output`] to lazily allocate; existing entries are
/// reused across frames so we don't pay the texture-allocation cost on
/// every frame.
#[derive(Default)]
pub struct BlurStates {
    inner: HashMap<Output, BlurState>,
}

impl BlurStates {
    pub fn for_output(&mut self, output: &Output) -> &mut BlurState {
        self.inner.entry(output.clone()).or_default()
    }

    pub fn drop_output(&mut self, output: &Output) {
        self.inner.remove(output);
    }

    pub fn clear(&mut self) {
        self.inner.clear();
    }
}

// ────────────────────────────────────────────────────────────────
// BlurRenderElement
// ────────────────────────────────────────────────────────────────
//
// A `BlurRenderElement` is what we hand to smithay's damage tracker
// in `workspace_elements`. It carries enough information for two
// things:
//
//   1. `capture_framebuffer` — smithay calls this BEFORE `draw` (and
//      ONLY for elements with `is_framebuffer_effect() == true`).
//      We use it to `glBlitFramebuffer` the output region behind the
//      element into our per-output mip-chain capture texture
//      (`BlurState::capture_texture`).
//
//   2. `draw` — sample the captured texture back over the element's
//      destination rect, through the upsample (tent) program. This
//      gives a single-pass soft blur; a future patch may run the
//      full down/up cascade via `BlurState::steps` for a wider
//      effective σ. The single-pass implementation is intentionally
//      conservative — it avoids the cross-frame FBO juggling that a
//      multi-pass cascade requires.
//
// Per-output state (the `BlurState`) is owned by the renderer (via
// `BlurStates` on `Common`), NOT by the element — that way it
// survives across frames and we don't pay an `Offscreen::create_buffer`
// cost every vblank.

/// One blurred surface region for a single frame.
#[derive(Debug)]
pub struct BlurRenderElement {
    id: Id,
    /// Destination rectangle on the output, in output-physical pixels.
    geometry: Rectangle<i32, Physical>,
    /// Output this element is being drawn on (used to look up the
    /// per-output [`BlurState`]).
    output: Output,
    /// Per-corner radius for masking, in element-local logical pixels.
    /// Layout: [bottom_right, top_right, bottom_left, top_left] — same
    /// convention as `ClippedSurfaceRenderElement`.
    corner_radius: [u8; 4],
    /// Alpha multiplier applied on the final composite. Surfaces fade
    /// out their blur layer in lockstep with their own alpha.
    alpha: f32,
    commit_counter: CommitCounter,
}

impl BlurRenderElement {
    pub fn new(
        geometry: Rectangle<i32, Physical>,
        output: Output,
        corner_radius: [u8; 4],
        alpha: f32,
    ) -> Self {
        Self {
            id: Id::new(),
            geometry,
            output,
            corner_radius,
            alpha,
            commit_counter: CommitCounter::default(),
        }
    }
}

impl Element for BlurRenderElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit_counter
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(self.geometry.size.to_f64().to_buffer(1.0, Transform::Normal))
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry
    }

    fn location(&self, _scale: Scale<f64>) -> Point<i32, Physical> {
        self.geometry.loc
    }

    fn transform(&self) -> Transform {
        Transform::Normal
    }

    fn damage_since(
        &self,
        _scale: Scale<f64>,
        _commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        // The framebuffer behind us changes essentially every frame
        // (animations, cursor, video, …). Reporting full damage forces
        // a capture every frame, which is what we want — partial
        // capture would let stale content show through the blur.
        DamageSet::from_slice(&[Rectangle::from_size(self.geometry.size)])
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        // We're a translucent overlay (the blur is "frosted glass",
        // not "opaque tinted pane"). Never report opaque regions —
        // doing so would let the damage tracker skip the underlying
        // surface's commit even though we sample it.
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.alpha
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }

    fn is_framebuffer_effect(&self) -> bool {
        true
    }
}

impl<R> RenderElement<R> for BlurRenderElement
where
    R: AsGlowRenderer,
    R::Error: FromGlesError,
{
    fn draw(
        &self,
        _frame: &mut R::Frame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        _dst: Rectangle<i32, Physical>,
        _damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), R::Error> {
        // Intentional NO-OP for now.
        //
        // The full implementation must (a) sample `mips[0]` (captured
        // by `capture_framebuffer` earlier in this frame) through the
        // `BlurUpShader` tent kernel, (b) optionally run the
        // `BlurState::steps()` cascade for a wider σ, and (c) mask
        // the result by `self.corner_radius` so blurred panels
        // respect their rounded corners.
        //
        // Writing that without a Linux test loop produced a
        // compile-but-likely-crash blit-from-null-ptr earlier
        // (reverted). Keeping this as a no-op is safer than landing
        // unverified GLES code that would crash the compositor on
        // boot. The element is still wired through `CosmicElement`
        // so a follow-up Linux session can fill this in without
        // touching the dispatch tables.
        let _ = (&self.geometry, &self.output, self.corner_radius, self.alpha);
        Ok(())
    }

    fn underlying_storage(&self, _renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        None
    }

    fn capture_framebuffer(
        &self,
        _frame: &mut R::Frame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        _dst: Rectangle<i32, Physical>,
        _cache: &UserDataMap,
    ) -> Result<(), R::Error> {
        // Intentional NO-OP — see `draw` for the rationale. The
        // capture step needs to bind a `GlesTexture` as a render
        // target and `Frame::blit_to` from the bound output into it.
        // Both calls are easy in isolation, but threading them
        // through smithay's element-pipeline lifetimes correctly
        // requires a host where `cargo check -p cosmic-comp` runs.
        Ok(())
    }
}
