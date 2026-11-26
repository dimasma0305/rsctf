import { GLOBE_RADIUS, type GlobePoint } from './arenaGlobeModel'

// Rasterize the existing impact mesh directly. The separate alpha canvas lets
// the browser composite the illustrated map and keeps team hit targets intact.
export function createImpactGpu(
  bed: HTMLCanvasElement,
  directions: GlobePoint[],
  faces: number[][],
  tile: Uint8Array<ArrayBuffer>,
  palette: Uint8Array<ArrayBuffer>
) {
  const canvas = document.createElement('canvas')
  const gl = canvas.getContext('webgl', {
    alpha: true,
    antialias: false,
    premultipliedAlpha: false,
    failIfMajorPerformanceCaveat: true,
  })
  if (!gl) return
  const program = gl.createProgram()
  const buffers: WebGLBuffer[] = []
  const textures: WebGLTexture[] = []
  const release = () => {
    for (const buffer of buffers) gl.deleteBuffer(buffer)
    for (const texture of textures) gl.deleteTexture(texture)
    gl.deleteProgram(program)
    gl.getExtension('WEBGL_lose_context')?.loseContext()
    canvas.remove()
  }
  if (!program) {
    release()
    return
  }
  const shader = (type: number, source: string) => {
    const s = gl.createShader(type)
    if (!s) return
    gl.shaderSource(s, source)
    gl.compileShader(s)
    gl.attachShader(program, s)
    gl.deleteShader(s)
  }
  shader(
    gl.VERTEX_SHADER,
    `
    attribute vec3 position, direction, material;
    varying highp vec3 rock, directionAt;
    void main() {
      directionAt = direction;
      rock = material;
      gl_Position = vec4(position.xy * vec2(${(2 * GLOBE_RADIUS) / 1000}, -${(2 * GLOBE_RADIUS) / 1000}), -position.z * 0.8, 1.);
    }`
  )
  shader(
    gl.FRAGMENT_SHADER,
    `
    precision highp float;
    uniform sampler2D minerals, colors;
    varying highp vec3 rock, directionAt;
    void main() {
      vec3 n = normalize(directionAt);
      vec2 uv = vec2(atan(n.x, n.z) / 6.2831853 + 0.5, asin(clamp(n.y, -1., 1.)) / 3.14159265 + 0.5);
      float mineral = floor(texture2D(minerals, uv * vec2(5., 3.)).r * 255. + 0.5);
      float light = floor(clamp(rock.x, 0., 1.) * 31. + 0.5);
      float heat = clamp(floor(rock.y * 49. + 0.5), 0., 63.);
      vec3 rgb = texture2D(colors, (vec2(light * 32. + mineral, heat) + 0.5) / vec2(1024., 64.)).rgb;
      gl_FragColor = vec4(rgb, rock.z);
    }`
  )
  gl.linkProgram(program)
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    release()
    return
  }
  gl.useProgram(program)
  function buffer(target: number, data: Float32Array<ArrayBuffer> | Uint16Array<ArrayBuffer>, usage: number) {
    const handle = gl!.createBuffer()
    if (!handle) return null
    buffers.push(handle)
    gl!.bindBuffer(target, handle)
    gl!.bufferData(target, data, usage)
    return handle
  }
  function attribute(name: string, stride: number, offset: number) {
    const at = gl!.getAttribLocation(program!, name)
    gl!.enableVertexAttribArray(at)
    gl!.vertexAttribPointer(at, 3, gl!.FLOAT, false, stride, offset)
  }
  const directionBuffer = buffer(
    gl.ARRAY_BUFFER,
    Float32Array.from(directions.flatMap((v) => [v.x, v.y, v.z])),
    gl.STATIC_DRAW
  )
  if (!directionBuffer) {
    release()
    return
  }
  attribute('direction', 0, 0)
  const vertices = new Float32Array(directions.length * 6)
  const streaming = buffer(gl.ARRAY_BUFFER, vertices, gl.DYNAMIC_DRAW)
  const indices = Uint16Array.from(faces.flat())
  if (!streaming || !buffer(gl.ELEMENT_ARRAY_BUFFER, indices, gl.STATIC_DRAW)) {
    release()
    return
  }
  attribute('position', 24, 0)
  attribute('material', 24, 12)
  const texture = (name: string, unit: number, format: number, w: number, h: number, data: Uint8Array<ArrayBuffer>) => {
    const tex = gl.createTexture()
    if (!tex) return false
    textures.push(tex)
    gl.activeTexture(gl.TEXTURE0 + unit)
    gl.bindTexture(gl.TEXTURE_2D, tex)
    gl.texImage2D(gl.TEXTURE_2D, 0, format, w, h, 0, format, gl.UNSIGNED_BYTE, data)
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST)
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST)
    gl.uniform1i(gl.getUniformLocation(program, name), unit)
    return true
  }
  if (
    !texture('minerals', 0, gl.LUMINANCE, 128, 128, tile) ||
    !texture('colors', 1, gl.RGB, 1024, 64, palette) ||
    gl.getError() !== gl.NO_ERROR
  ) {
    release()
    return
  }
  gl.enable(gl.DEPTH_TEST)
  gl.enable(gl.CULL_FACE)
  gl.frontFace(gl.CW)
  canvas.className = bed.className
  canvas.setAttribute('aria-hidden', 'true')
  bed.after(canvas)
  return {
    vertices,
    lost: () => gl.isContextLost(),
    clear: () => gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT),
    render(size: number) {
      if (canvas.width !== size || canvas.height !== size) canvas.width = canvas.height = size
      gl.viewport(0, 0, size, size)
      gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT)
      gl.bufferSubData(gl.ARRAY_BUFFER, 0, vertices)
      gl.drawElements(gl.TRIANGLES, indices.length, gl.UNSIGNED_SHORT, 0)
    },
    release,
  }
}
