// Dual-Kawase blur UPSAMPLE fragment shader.
//
// Taken from niri (GPL-3.0):
//   https://github.com/YaLTeR/niri/blob/db49deb/src/render_helpers/shaders/blur_up.frag
//
// 8-tap tent filter:
//   4 edge midpoints at +/- (2 * half_pixel * offset), weight 1
//   4 diagonal corners at +/- (half_pixel * offset),   weight 2
// Sum / 12. Run from the smallest mip back up to full size.
//
// `half_pixel` is `0.5 / source_size` during upsample (NB: source, not dest).
// `offset` is the same user-tunable Kawase radius multiplier as the
// downsample pass.

precision highp float;

varying vec2 v_coords;
uniform sampler2D tex;
uniform vec2 half_pixel;
uniform float offset;

void main() {
    vec2 o = half_pixel * offset;

    vec4 sum = vec4(0.0);
    sum += texture2D(tex, v_coords + vec2(-o.x * 2.0, 0.0));
    sum += texture2D(tex, v_coords + vec2( o.x * 2.0, 0.0));
    sum += texture2D(tex, v_coords + vec2(0.0, -o.y * 2.0));
    sum += texture2D(tex, v_coords + vec2(0.0,  o.y * 2.0));

    sum += texture2D(tex, v_coords + vec2(-o.x,  o.y)) * 2.0;
    sum += texture2D(tex, v_coords + vec2( o.x,  o.y)) * 2.0;
    sum += texture2D(tex, v_coords + vec2(-o.x, -o.y)) * 2.0;
    sum += texture2D(tex, v_coords + vec2( o.x, -o.y)) * 2.0;

    gl_FragColor = sum / 12.0;
}
