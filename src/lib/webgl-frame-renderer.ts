/**
 * WebGL 帧渲染器：对照 scrcpy 的 OpenGL 纹理渲染（SDL_RenderTexture），
 * 用 GPU 纹理上传替代 2D putImageData 全量像素拷贝，显著降低 CPU/GPU 峰值，
 * 避免高帧率大帧导致的 WebView 渲染进程崩溃（黑屏）。
 *
 * 失败时由调用方回退到 2D 路径。
 */

export interface FrameBytes {
  bytes: Uint8Array
  w: number
  h: number
}

const VERT_SRC = `
  attribute vec2 a_pos;
  varying vec2 v_uv;
  void main() {
    v_uv = a_pos * 0.5 + 0.5;
    gl_Position = vec4(a_pos, 0.0, 1.0);
  }
`

const FRAG_SRC = `
  precision mediump float;
  varying vec2 v_uv;
  uniform sampler2D u_tex;
  void main() {
    gl_FragColor = texture2D(u_tex, v_uv);
  }
`

export class WebGLFrameRenderer {
  private gl: WebGLRenderingContext | null = null
  private program: WebGLProgram | null = null
  private buf: WebGLBuffer | null = null
  private tex: WebGLTexture | null = null
  private textureInitialized = false
  private width = 0
  private height = 0

  static tryCreate(canvas: HTMLCanvasElement): WebGLFrameRenderer | null {
    const r = new WebGLFrameRenderer()
    return r.init(canvas) ? r : null
  }

  private init(canvas: HTMLCanvasElement): boolean {
    const gl = (canvas.getContext("webgl", { preserveDrawingBuffer: false }) ||
      canvas.getContext("experimental-webgl")) as WebGLRenderingContext | null
    if (!gl) return false
    this.gl = gl

    const compile = (type: number, src: string): WebGLShader | null => {
      const sh = gl.createShader(type)
      if (!sh) return null
      gl.shaderSource(sh, src)
      gl.compileShader(sh)
      if (!gl.getShaderParameter(sh, gl.COMPILE_STATUS)) {
        console.warn("shader compile failed:", gl.getShaderInfoLog(sh))
        gl.deleteShader(sh)
        return null
      }
      return sh
    }
    const vs = compile(gl.VERTEX_SHADER, VERT_SRC)
    const fs = compile(gl.FRAGMENT_SHADER, FRAG_SRC)
    if (!vs || !fs) return false
    const prog = gl.createProgram()
    if (!prog) return false
    gl.attachShader(prog, vs)
    gl.attachShader(prog, fs)
    gl.linkProgram(prog)
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) {
      console.warn("program link failed:", gl.getProgramInfoLog(prog))
      return false
    }
    this.program = prog
    gl.useProgram(prog)

    // 全屏三角形（3 顶点覆盖裁剪空间）
    this.buf = gl.createBuffer()
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buf)
    gl.bufferData(
      gl.ARRAY_BUFFER,
      new Float32Array([-1, -1, 3, -1, -1, 3]),
      gl.STATIC_DRAW,
    )
    const loc = gl.getAttribLocation(prog, "a_pos")
    gl.enableVertexAttribArray(loc)
    gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0)

    gl.uniform1i(gl.getUniformLocation(prog, "u_tex"), 0)
    return true
  }

  /** 上传一帧并绘制；尺寸变化时重建纹理。失败返回 false（调用方回退 2D）。 */
  render(frame: FrameBytes): boolean {
    const gl = this.gl
    if (!gl || !this.program) return false
    const { bytes, w, h } = frame
    if (w <= 0 || h <= 0) return false
    const need = w * h * 4
    if (bytes.byteLength < need) return false

    if (this.width !== w || this.height !== h) {
      if (this.tex) gl.deleteTexture(this.tex)
      this.tex = gl.createTexture()
      this.width = w
      this.height = h
      this.textureInitialized = false
    }
    if (!this.tex) return false

    gl.viewport(0, 0, w, h)
    gl.bindTexture(gl.TEXTURE_2D, this.tex)
    // RGBA 行序 top-to-bottom 与 WebGL 纹理坐标 (0,0)=左下 方向相反 → flipY
    gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, 1)
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1)
    if (!this.textureInitialized) {
      gl.texImage2D(
        gl.TEXTURE_2D,
        0,
        gl.RGBA,
        w,
        h,
        0,
        gl.RGBA,
        gl.UNSIGNED_BYTE,
        null,
      )
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR)
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR)
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE)
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE)
      this.textureInitialized = true
    }
    gl.texSubImage2D(
      gl.TEXTURE_2D,
      0,
      0,
      0,
      w,
      h,
      gl.RGBA,
      gl.UNSIGNED_BYTE,
      bytes.subarray(0, need),
    )
    gl.drawArrays(gl.TRIANGLES, 0, 3)
    return true
  }

  dispose(): void {
    const gl = this.gl
    if (!gl) return
    if (this.tex) gl.deleteTexture(this.tex)
    if (this.buf) gl.deleteBuffer(this.buf)
    if (this.program) gl.deleteProgram(this.program)
    this.gl = null
    this.tex = null
    this.textureInitialized = false
    this.buf = null
    this.program = null
  }
}
