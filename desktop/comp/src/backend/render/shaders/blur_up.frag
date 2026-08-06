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
//
// ClawOS addition: `saturation`.
//
// Averaging neighbours pulls every sample toward the local mean, which drains
// chroma as well as detail — a blurred wallpaper reads grey and the glass over
// it looks dirty rather than translucent. Pushing saturation back up on the
// final upsample restores the colour the average removed, which is what makes
// frosted glass read as glass. 1.0 leaves the result untouched.

precision highp float;

varying vec2 v_coords;
uniform sampler2D tex;
uniform vec2 half_pixel;
uniform float offset;
uniform float saturation;

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

    vec4 color = sum / 12.0;

    if (saturation != 1.0) {
        // Rec.709 luma, so pushing chroma does not also shift brightness.
        float luma = dot(color.rgb, vec3(0.2126, 0.7152, 0.0722));
        color.rgb = clamp(mix(vec3(luma), color.rgb, saturation), 0.0, 1.0);
    }

    gl_FragColor = color;
}
