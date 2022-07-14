#version 100
#extension GL_EXT_blend_func_extended: require
#define COLORED 1

varying mediump vec2 v_UV;
varying mediump float v_Flags;

uniform sampler2D u_Texture;

void main() {
    if (v_Flags == 1.) {
        // Color glyphs, like emojis.
        gl_FragColor = texture2D(u_Texture, v_UV);
        gl_SecondaryFragColorEXT = vec4(gl_FragColor.a);

        // Revert alpha premultiplication.
        if (gl_FragColor.a != 0.0) {
            gl_FragColor.rgb = vec3(gl_FragColor.rgb / gl_FragColor.a);
        }

        gl_FragColor = vec4(gl_FragColor.rgb, 1.0);
    } else {
        // Regular text glyphs.
        mediump vec3 textColor = texture2D(u_Texture, v_UV).rgb;
        gl_SecondaryFragColorEXT = vec4(textColor, textColor.r);
        gl_FragColor = vec4(1.0, 1.0, 1.0, 1.0);
    }
}
