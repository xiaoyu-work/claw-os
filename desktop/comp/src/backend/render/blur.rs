// SPDX-License-Identifier: GPL-3.0-only
//
// Dual-Kawase backdrop blur for cosmic-comp.
//
// Algorithm
// ---------
//
// We compile our own raw-GL blur-down + blur-up programs (smithay's
// `compile_custom_texture_shader` returns an opaque
// `GlesTexProgram` that doesn't expose the underlying `GLuint` or
// uniform locations, so we can't direct it at arbitrary FBOs from
// inside `with_context`). The cascade is the classic Bjørge
// dual-Kawase pyramid:
//
//   1. `capture_framebuffer` blits the output FB region behind the
//      surface into `mips[0]` (full resolution).
//   2. `passes` down-passes draw `mips[i] -> mips[i+1]`,
//      progressively halving the working resolution.
//   3. `passes` up-passes draw `mips[i+1] -> mips[i]`, applying the
//      8-tap upsample kernel and accumulating into the lower mip.
//   4. `mips[0]` now holds the fully-blurred image; `draw` samples
//      it back over `dst` via `render_texture_from_to`.
//
// All raw GL work happens inside `frame.with_context(|gl| unsafe …)`.
// Per the smithay contract we save + restore framebuffer binding,
// viewport, blending state, vertex-attrib state, and texture binding
// because smithay does NOT restore state for you between
// `with_context` calls.
//
// Reference (the cascade pattern is mirrored from niri):
//   Marius Bjørge, "Bandwidth-Efficient Rendering", SIGGRAPH 2015.
//   https://github.com/YaLTeR/niri/blob/main/src/render_helpers/blur.rs
//
// Integration
// -----------
//
//   The compiled `BlurPrograms` is stashed in `EglContext::user_data()`
//   once on first init by `init_blur_shaders` (called from
//   `render::init_shaders`). Per-element state — the mip chain —
//   lives in the smithay-managed per-element `cache: &UserDataMap`
//   so it persists across frames without external bookkeeping.
//
//   Gated by `AppearanceConfig::experimental_blur` (default `true`).
//   Errors during shader compile or texture allocation are logged
//   and swallowed; the worst case is the blur becomes transparent
//   (no-op) and the surface composites as if blur were disabled.

use std::cell::RefCell;
use std::cmp::max;
use std::collections::HashMap;
use std::rc::Rc;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Frame, FrameContext, Offscreen, Texture,
            element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
            gles::{
                GlesError, GlesFrame, GlesRenderer, GlesTexture,
                ffi, link_program,
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

pub static BLUR_VERT_SHADER: &str = include_str!("./shaders/blur.vert");
pub static BLUR_DOWN_SHADER: &str = include_str!("./shaders/blur_down.frag");
pub static BLUR_UP_SHADER: &str = include_str!("./shaders/blur_up.frag");

pub const DEFAULT_PASSES: usize = 4;
pub const DEFAULT_OFFSET: f32 = 1.5;

// ────────────────────────────────────────────────────────────────
// Compiled GL programs (raw — we need program IDs + uniform
// locations to drive arbitrary FBO targets per pass)
// ────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct BlurProgramInternal {
    program: ffi::types::GLuint,
    uniform_tex: ffi::types::GLint,
    uniform_half_pixel: ffi::types::GLint,
    uniform_offset: ffi::types::GLint,
    attrib_vert: ffi::types::GLint,
}

/// Compiled dual-Kawase down + up programs. Stored once per
/// `GlesRenderer` in its `EglContext::user_data()`.
#[derive(Debug, Clone)]
pub struct BlurPrograms(Rc<BlurProgramsInner>);

#[derive(Debug)]
struct BlurProgramsInner {
    down: BlurProgramInternal,
    up: BlurProgramInternal,
}

unsafe fn compile_blur(
    gl: &ffi::Gles2,
    frag_src: &str,
) -> Result<BlurProgramInternal, GlesError> {
    let program = link_program(gl, BLUR_VERT_SHADER, frag_src)?;

    let tex = c"tex";
    let half_pixel = c"half_pixel";
    let offset = c"offset";
    let vert = c"vert";

    Ok(BlurProgramInternal {
        program,
        uniform_tex: gl.GetUniformLocation(program, tex.as_ptr()),
        uniform_half_pixel: gl.GetUniformLocation(program, half_pixel.as_ptr()),
        uniform_offset: gl.GetUniformLocation(program, offset.as_ptr()),
        attrib_vert: gl.GetAttribLocation(program, vert.as_ptr()),
    })
}

// ────────────────────────────────────────────────────────────────
// Shader compilation entry-point
// ────────────────────────────────────────────────────────────────

pub fn init_blur_shaders(renderer: &mut GlesRenderer) {
    {
        let ud = renderer.egl_context().user_data();
        if ud.get::<BlurPrograms>().is_some() {
            return;
        }
    }

    let result = renderer.with_context(|gl| unsafe {
        let down = compile_blur(gl, BLUR_DOWN_SHADER)?;
        let up = compile_blur(gl, BLUR_UP_SHADER)?;
        Ok::<_, GlesError>(BlurPrograms(Rc::new(BlurProgramsInner { down, up })))
    });

    let programs = match result {
        Ok(Ok(p)) => p,
        Ok(Err(err)) => {
            tracing::warn!(?err, "blur shaders failed to compile; blur disabled");
            return;
        }
        Err(err) => {
            tracing::warn!(?err, "blur shaders: failed to make GL context current");
            return;
        }
    };

    let ud = renderer.egl_context().user_data();
    ud.insert_if_missing(|| programs);
    tracing::info!(
        "dual-Kawase blur shaders compiled (passes={}, offset={})",
        DEFAULT_PASSES,
        DEFAULT_OFFSET
    );
}

// ────────────────────────────────────────────────────────────────
// Per-output state (kept for compatibility; no longer the primary
// home for the mip chain, which now lives per-element)
// ────────────────────────────────────────────────────────────────

pub struct BlurState {
    pub mips: Vec<GlesTexture>,
    pub size: Size<i32, Physical>,
    pub format: Fourcc,
    pub passes: usize,
    pub offset: f32,
}

impl std::fmt::Debug for BlurState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlurState")
            .field("mips_len", &self.mips.len())
            .field("size", &self.size)
            .field("format", &self.format)
            .field("passes", &self.passes)
            .field("offset", &self.offset)
            .finish()
    }
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
        ud.get::<BlurPrograms>().is_some()
    }

    pub fn output_texture(&self) -> Option<&GlesTexture> {
        self.mips.first()
    }
}

#[derive(Debug, Default)]
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
///
/// `mips[0]` is the full-resolution capture / final cascade output;
/// `mips[i]` for `i >= 1` is half the size of `mips[i-1]`.
struct BlurInner {
    mips: Vec<GlesTexture>,
    /// `true` once the cascade has run successfully in the current
    /// frame; `do_draw` only samples back if this is set.
    ready: bool,
}

impl BlurInner {
    fn new() -> Self {
        Self {
            mips: Vec::new(),
            ready: false,
        }
    }

    /// (Re)allocate the mip chain so it matches `top_size` in buffer
    /// coords, with `passes + 1` total textures decreasing by 2 each
    /// step (clamped to a minimum of 1 pixel).
    fn prepare(
        &mut self,
        renderer: &mut GlesRenderer,
        top_size: Size<i32, Buffer>,
        passes: usize,
    ) -> Result<(), GlesError> {
        let mut needed = Vec::with_capacity(passes + 1);
        let mut w = top_size.w;
        let mut h = top_size.h;
        for _ in 0..=passes {
            needed.push(Size::<i32, Buffer>::from((w, h)));
            w = max(1, w / 2);
            h = max(1, h / 2);
        }

        let same_layout = self.mips.len() == needed.len()
            && self
                .mips
                .iter()
                .zip(&needed)
                .all(|(t, s)| t.size() == *s);

        if !same_layout {
            self.mips.clear();
            for size in &needed {
                let tex = Offscreen::<GlesTexture>::create_buffer(
                    renderer,
                    Fourcc::Abgr8888,
                    *size,
                )?;
                self.mips.push(tex);
            }
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
    /// Number of down/up cascade passes (>= 1). Higher = wider
    /// effective blur radius for the same shader cost (because
    /// passes operate on progressively smaller mips).
    passes: usize,
    /// Kawase tap offset, in half-pixels. ~1.0 ‒ 2.0 is typical;
    /// our default `1.5` matches niri's default.
    offset: f32,
}

impl std::fmt::Debug for BlurRenderElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlurRenderElement")
            .field("id", &self.id)
            .field("geometry", &self.geometry)
            .field("corner_radius", &self.corner_radius)
            .field("alpha", &self.alpha)
            .field("passes", &self.passes)
            .field("offset", &self.offset)
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
            passes: DEFAULT_PASSES,
            offset: DEFAULT_OFFSET,
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
        Rectangle::from_size(
            self.geometry
                .size
                .to_logical(1)
                .to_buffer(1, Transform::Normal)
                .to_f64(),
        )
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
        inner.ready = false;

        // Clamp dst to the output (BlitFramebuffer skips OOB pixels
        // but we'd rather not allocate a texture larger than what
        // we'll actually read).
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

        // Pull the compiled programs first — if shaders failed to
        // compile we don't even bother allocating textures.
        let programs = {
            let mut guard = frame.renderer();
            let r = guard.as_mut();
            match r.egl_context().user_data().get::<BlurPrograms>() {
                Some(p) => p.clone(),
                None => return Ok(()),
            }
        };

        let passes = self.passes.clamp(1, 8);

        // Allocate / reuse mip chain.
        {
            let mut guard = frame.renderer();
            inner.prepare(guard.as_mut(), cap_size, passes)?;
        }

        // We're going to run the entire cascade in `with_context`.
        // All texture references it touches need to outlive the
        // closure; clone them up-front.
        let mips: Vec<GlesTexture> = inner.mips.clone();
        if mips.is_empty() {
            return Ok(());
        }

        let read_x = dst_xformed.loc.x;
        let read_y = dst_xformed.loc.y;
        let read_w = dst_xformed.size.w;
        let read_h = dst_xformed.size.h;
        let offset = self.offset;

        frame.with_context(|gl| unsafe {
            while gl.GetError() != ffi::NO_ERROR {}

            // ─── Save state we'll touch ────────────────────────
            let mut prev_draw_fbo: i32 = 0;
            let mut prev_read_fbo: i32 = 0;
            let mut prev_viewport: [i32; 4] = [0; 4];
            let mut prev_program: i32 = 0;
            let mut prev_tex_2d: i32 = 0;
            let mut prev_array_buffer: i32 = 0;
            let mut prev_active_texture: i32 = 0;
            let prev_blend = gl.IsEnabled(ffi::BLEND) != 0;
            let prev_scissor = gl.IsEnabled(ffi::SCISSOR_TEST) != 0;
            gl.GetIntegerv(ffi::DRAW_FRAMEBUFFER_BINDING, &mut prev_draw_fbo);
            gl.GetIntegerv(ffi::READ_FRAMEBUFFER_BINDING, &mut prev_read_fbo);
            gl.GetIntegerv(ffi::VIEWPORT, prev_viewport.as_mut_ptr());
            gl.GetIntegerv(ffi::CURRENT_PROGRAM, &mut prev_program);
            gl.GetIntegerv(ffi::ACTIVE_TEXTURE, &mut prev_active_texture);
            gl.ActiveTexture(ffi::TEXTURE0);
            gl.GetIntegerv(ffi::TEXTURE_BINDING_2D, &mut prev_tex_2d);
            gl.GetIntegerv(ffi::ARRAY_BUFFER_BINDING, &mut prev_array_buffer);

            gl.Disable(ffi::BLEND);
            gl.Disable(ffi::SCISSOR_TEST);

            // ─── Step 1: blit output FB region into mips[0] ────
            let mut fbo: u32 = 0;
            gl.GenFramebuffers(1, &mut fbo);
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, fbo);
            gl.FramebufferTexture2D(
                ffi::DRAW_FRAMEBUFFER,
                ffi::COLOR_ATTACHMENT0,
                ffi::TEXTURE_2D,
                mips[0].tex_id(),
                0,
            );
            // READ_FRAMEBUFFER stays bound to whatever smithay had
            // (i.e. the output's compositing target).
            gl.BlitFramebuffer(
                read_x,
                read_y,
                read_x + read_w,
                read_y + read_h,
                0,
                0,
                cap_size.w,
                cap_size.h,
                ffi::COLOR_BUFFER_BIT,
                ffi::LINEAR,
            );

            // ─── Step 2: down/up cascade ───────────────────────
            //
            // Vertex setup: bind a unit-quad attribute array. We
            // re-use the same data for every pass — only the
            // sampler / FBO / viewport / uniforms change.
            #[rustfmt::skip]
            let verts: [f32; 12] = [
                0.0, 0.0,  0.0, 1.0,  1.0, 1.0,
                0.0, 0.0,  1.0, 1.0,  1.0, 0.0,
            ];
            gl.BindBuffer(ffi::ARRAY_BUFFER, 0);

            // ─── Down pass: mips[i] → mips[i+1] ────────────────
            let down = &programs.0.down;
            gl.UseProgram(down.program);
            gl.Uniform1i(down.uniform_tex, 0);
            gl.Uniform1f(down.uniform_offset, offset);
            gl.EnableVertexAttribArray(down.attrib_vert as u32);
            gl.VertexAttribPointer(
                down.attrib_vert as u32,
                2,
                ffi::FLOAT,
                ffi::FALSE,
                0,
                verts.as_ptr().cast(),
            );

            for i in 0..passes {
                let src_tex = &mips[i];
                let dst_tex = &mips[i + 1];
                let ds = dst_tex.size();

                gl.Viewport(0, 0, ds.w, ds.h);
                // During downsample, `half_pixel` is half of the
                // DEST pixel (`offset` then expands the sampling
                // radius outward in source space).
                gl.Uniform2f(
                    down.uniform_half_pixel,
                    0.5 / ds.w as f32,
                    0.5 / ds.h as f32,
                );

                gl.FramebufferTexture2D(
                    ffi::DRAW_FRAMEBUFFER,
                    ffi::COLOR_ATTACHMENT0,
                    ffi::TEXTURE_2D,
                    dst_tex.tex_id(),
                    0,
                );
                gl.BindTexture(ffi::TEXTURE_2D, src_tex.tex_id());
                gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
                gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
                gl.TexParameteri(
                    ffi::TEXTURE_2D,
                    ffi::TEXTURE_WRAP_S,
                    ffi::CLAMP_TO_EDGE as i32,
                );
                gl.TexParameteri(
                    ffi::TEXTURE_2D,
                    ffi::TEXTURE_WRAP_T,
                    ffi::CLAMP_TO_EDGE as i32,
                );

                gl.DrawArrays(ffi::TRIANGLES, 0, 6);
            }
            gl.DisableVertexAttribArray(down.attrib_vert as u32);

            // ─── Up pass: mips[i+1] → mips[i] ──────────────────
            let up = &programs.0.up;
            gl.UseProgram(up.program);
            gl.Uniform1i(up.uniform_tex, 0);
            gl.Uniform1f(up.uniform_offset, offset);
            gl.EnableVertexAttribArray(up.attrib_vert as u32);
            gl.VertexAttribPointer(
                up.attrib_vert as u32,
                2,
                ffi::FLOAT,
                ffi::FALSE,
                0,
                verts.as_ptr().cast(),
            );

            for i in (0..passes).rev() {
                let src_tex = &mips[i + 1];
                let dst_tex = &mips[i];
                let ds = dst_tex.size();
                let ss = src_tex.size();

                gl.Viewport(0, 0, ds.w, ds.h);
                // During upsample, `half_pixel` is half of the
                // SOURCE pixel (we're spreading the smaller mip out
                // over the next-larger texture).
                gl.Uniform2f(
                    up.uniform_half_pixel,
                    0.5 / ss.w as f32,
                    0.5 / ss.h as f32,
                );

                gl.FramebufferTexture2D(
                    ffi::DRAW_FRAMEBUFFER,
                    ffi::COLOR_ATTACHMENT0,
                    ffi::TEXTURE_2D,
                    dst_tex.tex_id(),
                    0,
                );
                gl.BindTexture(ffi::TEXTURE_2D, src_tex.tex_id());
                gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
                gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
                gl.TexParameteri(
                    ffi::TEXTURE_2D,
                    ffi::TEXTURE_WRAP_S,
                    ffi::CLAMP_TO_EDGE as i32,
                );
                gl.TexParameteri(
                    ffi::TEXTURE_2D,
                    ffi::TEXTURE_WRAP_T,
                    ffi::CLAMP_TO_EDGE as i32,
                );

                gl.DrawArrays(ffi::TRIANGLES, 0, 6);
            }
            gl.DisableVertexAttribArray(up.attrib_vert as u32);

            // ─── Restore state ─────────────────────────────────
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, prev_draw_fbo as u32);
            gl.BindFramebuffer(ffi::READ_FRAMEBUFFER, prev_read_fbo as u32);
            gl.Viewport(
                prev_viewport[0],
                prev_viewport[1],
                prev_viewport[2],
                prev_viewport[3],
            );
            gl.UseProgram(prev_program as u32);
            gl.BindTexture(ffi::TEXTURE_2D, prev_tex_2d as u32);
            gl.BindBuffer(ffi::ARRAY_BUFFER, prev_array_buffer as u32);
            gl.ActiveTexture(prev_active_texture as u32);
            if prev_blend {
                gl.Enable(ffi::BLEND);
            }
            if prev_scissor {
                gl.Enable(ffi::SCISSOR_TEST);
            }
            gl.DeleteFramebuffers(1, &fbo);

            Ok::<(), GlesError>(())
        })??;

        inner.ready = true;
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
        if !inner.ready || inner.mips.is_empty() {
            return Ok(());
        }

        let texture = &inner.mips[0];
        let tex_size = texture.size();
        if tex_size.w <= 0 || tex_size.h <= 0 {
            return Ok(());
        }

        // mips[0] already holds the fully-blurred image. Sample it
        // back through smithay's standard texture path — no
        // additional shader required (we're past the cascade).
        frame.render_texture_from_to(
            texture,
            Rectangle::from_size(tex_size.to_f64()),
            dst,
            damage,
            &[],
            Transform::Normal,
            self.alpha,
            None,
            &[],
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
