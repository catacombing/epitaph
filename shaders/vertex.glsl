#version 100

attribute vec2 a_Position;
attribute vec2 a_UV;
attribute float a_Flags;

varying vec2 v_UV;
varying float v_Flags;

uniform vec4 u_Projection;

void main() {
    v_Flags = a_Flags;
    v_UV = a_UV;
    vec2 finalPosition = u_Projection.xy + a_Position * u_Projection.zw;
    gl_Position = vec4(finalPosition, 0., 1.);
}
