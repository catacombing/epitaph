attribute vec2 aPos;
attribute vec4 aColor;

varying mediump vec4 color;

void main() {
    color = aColor;
    gl_Position = vec4(aPos.x, aPos.y, 0.0, 1.0);
}
