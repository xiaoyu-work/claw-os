// Dual-Kawase blur — fullscreen vertex shader.
//
// Taken from niri (GPL-3.0):
//   https://github.com/YaLTeR/niri/blob/db49deb/src/render_helpers/shaders/blur.vert
// Algorithm reference:
//   Marius Bjørge, "Bandwidth-Efficient Rendering", SIGGRAPH 2015.
//
// The mesh is a [0,1]^2 unit quad (two triangles); we remap to NDC inline so
// every blur pass can be drawn with a hard-coded 6-vertex DrawArrays call.
#version 100

attribute vec2 vert;
varying vec2 v_coords;

void main() {
    v_coords = vert;
    vec2 position = vert * 2.0 - 1.0;
    gl_Position = vec4(position, 1.0, 1.0);
}
