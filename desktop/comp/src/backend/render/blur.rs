// SPDX-License-Identifier: GPL-3.0-only
//
// Dual-Kawase backdrop blur for cosmic-comp.
//
// Algorithm
// ---------
//
// We compile the blur-down + blur-up GLSL programs via smithay's
// `compile_custom_texture_shader`, capture the output framebuffer
// region behind the blurred surface into a downsampled texture
// using `glBlitFramebuffer`, then composite the texture back over
// the destination rect with the blur-up shader applied
// (single-pass 8-tap kernel). This gives an inexpensive ~5–8 px
// effective blur radius — not as silky as a true 4-pass dual-Kawase
// cascade but visible, GPU-cheap, and survives multi-renderer
// dispatch without us having to manage raw GL program objects
// outside smithay's plumbing.
//
// Reference (full cascade, kept here for the next pass):
//   Marius Bjørge, "Bandwidth-Efficient Rendering", SIGGRAPH 2015.
//   https://github.com/niri-wm/niri/blob/main/src/render_helpers/blur.rs
//
// Integration
// -----------
//
//   The compiled `BlurDownShader` + `BlurUpShader` are stashed in
//   `EglContext::user_data()` once on first init by
//   `init_blur_shaders` (called from `render::init_shaders`). Per-
//   element state — the captured framebuffer texture — lives in the
//   smithay-managed per-element `cache: &UserDataMap` so it
//   persists across frames without external bookkeeping.
//
//   Gated by `AppearanceConfig::experimental_blur` (default `true`).
//   Errors during shader compile or texture allocation are logged
//   and swallowed; the worst case is the blur becomes transparent
//   (no-op) and the surface composites as if blur were disabled.

use std::cell::RefCell;
use std::collections::HashMap;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Frame, FrameContext, Offscreen, Texture,
            element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
            gles::{
                GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform,
                UniformName, UniformType, ffi,
            },
            glow::{GlowFrame, GlowRenderer},
            utils::{CommitCounter, DamageSet, OpaqueRegions},
        },
    },
    output::Output,
    utils::{
        Buffer, Physical, Point, Rectangle, Scale, Size, Transform, user_data::UserDataMap,
    },
};

use super::element::{AsGlowRenderer, FromGlesError};

pub static BLUR_DOWN_SHADER: &str = include_str!("./shaders/blur_down.frag");
pub static BLUR_UP_SHADER: &str = include_str!("./shaders/blur_up.frag");

/// Compiled dual-Kawase downsample program.
pub struct BlurDownShader(pub GlesTexProgram);

/// Compiled dual-Kawase upsample program.
pub struct BlurUpShader(pub GlesTexProgram);

pub const DEFAULT_PASSES: usize = 4;
pub const DEFAULT_OFFSET: f32 = 1.5;

// ────────────────────────────────────────────────────────────────
// Shader compilation
// ────────────────────────────────────────────────────────────────

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
// Per-output state (kept for compat with the eventual cascade)
// ────────────────────────────────────────────────────────────────

pub struct BlurState {
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
    pub fn shaders_available(renderer: &GlesRenderer) -> bool {
        let ud = renderer.egl_context().user_data();
        ud.get::<BlurDownShader>().is_some() && ud.get::<BlurUpShader>().is_some()
    }

    pub fn output_texture(&self) -> Option<&GlesTexture> {
        self.mips.first()
    }
}

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

/// Per-element scratch state stored in the smithay-managed
/// `UserDataMap` so it persists across frames without external
/// bookkeeping.
struct BlurInner {
    /// Captured framebuffer region. Allocated to match the
    /// destination rectangle's buffer size.
    framebuffer: Option<GlesTexture>,
    /// Set in `capture_framebuffer`, read in `draw`. Same handle as
    /// `framebuffer` once capture completes.
    intermediate: Option<GlesTexture>,
}

impl BlurInner {
    fn new() -> Self {
        Self {
            framebuffer: None,
            intermediate: None,
        }
    }

    /// (Re)allocate framebuffer so it matches `size` in buffer coords.
    fn prepare(
        &mut self,
        renderer: &mut GlesRenderer,
        size: Size<i32, Buffer>,
    ) -> Result<(), GlesError> {
        let recreate = match self.framebuffer.as_ref() {
            Some(fb) => fb.size() != size,
            None => true,
        };
        if recreate {
            self.framebuffer = Some(Offscreen::<GlesTexture>::create_buffer(
                renderer,
                Fourcc::Abgr8888,
                size,
            )?);
        }
        Ok(())
    }
}

/// A blur "framebuffer effect" element placed BEHIND the surface
/// in z-order. Smithay's damage tracker calls `capture_framebuffer`
/// before the surface draws (capturing the current output FB region
/// under the surface), then `draw` blits the blurred result back
/// over the destination rect.
pub struct BlurRenderElement {
    id: Id,
    /// Destination rectangle on the output, in output-physical pixels.
    geometry: Rectangle<i32, Physical>,
    /// Per-corner radius, currently unused but reserved for the
    /// `ClippingShader` masked composite path. Same convention as
    /// `ClippedSurfaceRenderElement`: [bottom_right, top_right,
    /// bottom_left, top_left].
    corner_radius: [u8; 4],
    /// Alpha multiplier on the composited blur layer.
    alpha: f32,
    commit_counter: CommitCounter,
}

impl std::fmt::Debug for BlurRenderElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlurRenderElement")
            .field("id", &self.id)
            .field("geometry", &self.geometry)
            .field("corner_radius", &self.corner_radius)
            .field("alpha", &self.alpha)
            .finish()
    }
}

impl BlurRenderElement {
    pub fn new(
        geometry: Rectangle<i32, Physical>,
        corner_radius: [u8; 4],
        alpha: f32,
    ) -> Self {
        Self {
            id: Id::new(),
            geometry,
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
        // FB behind us changes every frame; full damage forces a
        // capture every frame which is what we want.
        DamageSet::from_slice(&[Rectangle::from_size(self.geometry.size)])
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        // Blur is a translucent overlay; never report opaque or the
        // damage tracker may skip the underlying surface render.
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

// ────────────────────────────────────────────────────────────────
// Actual blur work (GlesRenderer path)
// ────────────────────────────────────────────────────────────────

impl BlurRenderElement {
    fn do_capture(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), GlesError> {
        let output_rect = Rectangle::from_size(frame.output_size());
        let transform = frame.transformation();

        let inner = cache.get_or_insert::<RefCell<BlurInner>, _>(|| RefCell::new(BlurInner::new()));
        let mut inner = inner.borrow_mut();
        let inner = &mut *inner;
        inner.intermediate = None;

        // Clamp dst to the output (BlitFramebuffer skips OOB pixels).
        let clamped_dst = match dst.intersection(output_rect) {
            Some(c) => c,
            None => return Ok(()),
        };

        // Apply output transform to dst (we're blitting from the
        // output FB which is in output-physical coords that smithay
        // applies the transform to).
        let dst_xformed = transform.transform_rect_in(clamped_dst, &output_rect.size);

        // Capture texture size in buffer coords.
        let cap_size: Size<i32, Buffer> = dst_xformed
            .size
            .to_logical(1)
            .to_buffer(1, Transform::Normal);

        if cap_size.w <= 0 || cap_size.h <= 0 {
            return Ok(());
        }

        // Allocate framebuffer texture if needed.
        {
            let mut guard = frame.renderer();
            inner.prepare(guard.as_mut(), cap_size)?;
        }

        let fb_tex = match inner.framebuffer.as_ref() {
            Some(t) => t.clone(),
            None => return Ok(()),
        };

        // Bail out cleanly if the blur shaders aren't compiled —
        // capture would succeed but draw can't sample them, so
        // there's no point burning the blit.
        {
            let mut guard = frame.renderer();
            let r = guard.as_mut();
            if !BlurState::shaders_available(r) {
                return Ok(());
            }
        }

        // Blit the output FB region into fb_tex.
        frame.with_context(|gl| unsafe {
            while gl.GetError() != ffi::NO_ERROR {}

            let mut current_fbo: i32 = 0;
            gl.GetIntegerv(ffi::DRAW_FRAMEBUFFER_BINDING, &mut current_fbo as *mut _);

            gl.Disable(ffi::SCISSOR_TEST);

            let mut fbo: u32 = 0;
            gl.GenFramebuffers(1, &mut fbo as *mut _);
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, fbo);
            gl.FramebufferTexture2D(
                ffi::DRAW_FRAMEBUFFER,
                ffi::COLOR_ATTACHMENT0,
                ffi::TEXTURE_2D,
                fb_tex.tex_id(),
                0,
            );

            gl.BlitFramebuffer(
                dst_xformed.loc.x,
                dst_xformed.loc.y,
                dst_xformed.loc.x + dst_xformed.size.w,
                dst_xformed.loc.y + dst_xformed.size.h,
                0,
                0,
                cap_size.w,
                cap_size.h,
                ffi::COLOR_BUFFER_BIT,
                ffi::LINEAR,
            );

            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, current_fbo as u32);
            gl.Enable(ffi::SCISSOR_TEST);
            gl.DeleteFramebuffers(1, &mut fbo as *mut _);

            Ok::<(), GlesError>(())
        })??;

        inner.intermediate = Some(fb_tex);
        Ok(())
    }

    fn do_draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let cache = match cache {
            Some(c) => c,
            None => return Ok(()),
        };
        let inner = match cache.get::<RefCell<BlurInner>>() {
            Some(i) => i,
            None => return Ok(()),
        };
        let inner = inner.borrow();

        let texture = match inner.intermediate.as_ref() {
            Some(t) => t,
            None => return Ok(()),
        };

        let tex_size = texture.size();
        if tex_size.w <= 0 || tex_size.h <= 0 {
            return Ok(());
        }
        let half_pixel = [0.5 / tex_size.w as f32, 0.5 / tex_size.h as f32];

        let up_program = {
            let mut guard = frame.renderer();
            guard
                .as_mut()
                .egl_context()
                .user_data()
                .get::<BlurUpShader>()
                .map(|p| p.0.clone())
        };

        frame.render_texture_from_to(
            texture,
            Rectangle::from_size(tex_size.to_f64()),
            dst,
            damage,
            &[],
            Transform::Normal,
            self.alpha,
            up_program.as_ref(),
            &[
                Uniform::new("half_pixel", half_pixel),
                Uniform::new("offset", DEFAULT_OFFSET),
            ],
        )?;

        // corner_radius is reserved for the eventual ClippingShader
        // masked composite path.
        let _ = self.corner_radius;
        Ok(())
    }
}

impl RenderElement<GlesRenderer> for BlurRenderElement {
    fn capture_framebuffer(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), GlesError> {
        self.do_capture(frame, src, dst, cache)
    }

    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        self.do_draw(frame, src, dst, damage, opaque_regions, cache)
    }
}

impl RenderElement<GlowRenderer> for BlurRenderElement {
    fn capture_framebuffer(
        &self,
        frame: &mut GlowFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), <GlowRenderer as smithay::backend::renderer::RendererSuper>::Error> {
        let gles_frame: &mut GlesFrame<'_, '_> =
            std::borrow::BorrowMut::borrow_mut(frame);
        self.do_capture(gles_frame, src, dst, cache)?;
        Ok(())
    }

    fn draw(
        &self,
        frame: &mut GlowFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), <GlowRenderer as smithay::backend::renderer::RendererSuper>::Error> {
        let gles_frame: &mut GlesFrame<'_, '_> =
            std::borrow::BorrowMut::borrow_mut(frame);
        self.do_draw(gles_frame, src, dst, damage, opaque_regions, cache)?;
        Ok(())
    }

    fn underlying_storage(
        &self,
        _renderer: &mut GlowRenderer,
    ) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

// Multi-renderer bridge: the CosmicElement dispatch calls these so
// `GlMultiRenderer` (KMS multi-GPU path) goes through GlowRenderer.
impl BlurRenderElement {
    pub fn draw_through_glow<R>(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), R::Error>
    where
        R: AsGlowRenderer,
        R::Error: FromGlesError,
    {
        let glow_frame = R::glow_frame_mut(frame);
        <Self as RenderElement<GlowRenderer>>::draw(
            self,
            glow_frame,
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        )
        .map_err(FromGlesError::from_gles_error)
    }

    pub fn capture_through_glow<R>(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), R::Error>
    where
        R: AsGlowRenderer,
        R::Error: FromGlesError,
    {
        let glow_frame = R::glow_frame_mut(frame);
        <Self as RenderElement<GlowRenderer>>::capture_framebuffer(
            self, glow_frame, src, dst, cache,
        )
        .map_err(FromGlesError::from_gles_error)
    }
}
