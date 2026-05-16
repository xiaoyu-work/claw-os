// Dual-Kawase blur DOWNSAMPLE fragment shader.
//
// Taken from niri (GPL-3.0):
//   https://github.com/YaLTeR/niri/blob/db49deb/src/render_helpers/shaders/blur_down.frag
//
// 5-tap kernel: centre weighted x4 + 4 diagonals at +/- (half_pixel * offset)
// each weighted x1. Divide by 8. Run from full-size source -> 1/2, 1/4, ...
// down the mip chain.
//
// `half_pixel` is `0.5 / dest_size` during downsample (NB: dest, not src).
// `offset` is the user-tunable Kawase radius multiplier (typically 1.0-2.0).

precision highp float;

varying vec2 v_coords;
uniform sampler2D tex;
uniform vec2 half_pixel;
uniform float offset;

void main() {
    vec2 o = half_pixel * offset;

    vec4 sum = texture2D(tex, v_coords) * 4.0;
    sum += texture2D(tex, v_coords + vec2(-o.x, -o.y));
    sum += texture2D(tex, v_coords + vec2( o.x, -o.y));
    sum += texture2D(tex, v_coords + vec2(-o.x,  o.y));
    sum += texture2D(tex, v_coords + vec2( o.x,  o.y));

    gl_FragColor = sum / 8.0;
}
