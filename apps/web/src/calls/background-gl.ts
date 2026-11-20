// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The camera background's drawing, on the GPU through WebGL2 (ADR-0089
 * section 2): the model's small input picture, a separable blur of the
 * down-scaled frame, and the blend of the frame over the blurred frame or
 * over a picture, weighted by the person mask.
 *
 * The parameters come from the shared rules (`lakko_segment`), so web,
 * desktop, iOS and Android draw the same thing: blur divisor, sigma and taps
 * per frame height, and the centred cover rectangle of a picture.
 *
 * Works on an `OffscreenCanvas` (the worker) and on an `HTMLCanvasElement`
 * (the main-thread canvas path). Browser-bound; not reachable from vitest.
 *
 * Texture orientation: an uploaded frame or mask has its first row (the top)
 * at t = 0. A pass into a texture used again keeps that (`flip = 1`), and so
 * does the model-input pass, whose `readPixels` then reads the top row first.
 * Only the pass onto the canvas flips (`flip = -1`), because a canvas shows
 * clip y = +1 at the top.
 *
 * The frame goes into the model input whole and upright with its shape kept
 * (`background-geometry.ts`, the shared rule), black around it; the mask is
 * read back through the same rectangle.
 */

import type { FrameCompositor } from "./background-engine";
import type { CameraFrame, StageMode } from "./background-mode";
import { maskUvRect, modelViewport, type ModelViewport } from "./background-geometry";
import { SEG_INPUT_HEIGHT, SEG_INPUT_WIDTH, type CompositeRules } from "./background-segment-wasm";

/** Taps on each side of the centre, at most (the rule's strongest blur at
 *  4096 px stays far below it). */
const MAX_BLUR_RADIUS = 64;

const VERTEX = `#version 300 es
uniform float uFlip;
out vec2 vUv;
void main() {
  vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
  vUv = p;
  vec2 clip = p * 2.0 - 1.0;
  gl_Position = vec4(clip.x, clip.y * uFlip, 0.0, 1.0);
}`;

const COPY = `#version 300 es
precision mediump float;
uniform sampler2D uTex;
in vec2 vUv;
out vec4 outColor;
void main() { outColor = vec4(texture(uTex, vUv).rgb, 1.0); }`;

const BLUR = `#version 300 es
precision mediump float;
uniform sampler2D uTex;
uniform vec2 uStep;
uniform float uSigma;
uniform int uRadius;
in vec2 vUv;
out vec4 outColor;
void main() {
  vec3 sum = vec3(0.0);
  float total = 0.0;
  float twoSigma2 = 2.0 * uSigma * uSigma;
  for (int i = -uRadius; i <= uRadius; ++i) {
    float w = exp(-float(i * i) / twoSigma2);
    sum += texture(uTex, vUv + uStep * float(i)).rgb * w;
    total += w;
  }
  outColor = vec4(sum / total, 1.0);
}`;

const BLEND = `#version 300 es
precision mediump float;
uniform sampler2D uFrame;
uniform sampler2D uBack;
uniform sampler2D uMask;
uniform vec4 uBackRect;
uniform vec4 uMaskRect;
in vec2 vUv;
out vec4 outColor;
void main() {
  float alpha = texture(uMask, uMaskRect.xy + vUv * uMaskRect.zw).r;
  vec3 frame = texture(uFrame, vUv).rgb;
  vec3 back = texture(uBack, uBackRect.xy + vUv * uBackRect.zw).rgb;
  outColor = vec4(frame * alpha + back * (1.0 - alpha), 1.0);
}`;

interface Program {
  readonly program: WebGLProgram;
  readonly uniforms: Map<string, WebGLUniformLocation | null>;
}

interface Target {
  readonly texture: WebGLTexture;
  readonly framebuffer: WebGLFramebuffer;
  width: number;
  height: number;
}

type Canvas = OffscreenCanvas | HTMLCanvasElement;

export class GlBackgroundCompositor implements FrameCompositor<CameraFrame, void, StageMode> {
  private readonly gl: WebGL2RenderingContext;
  private readonly copy: Program;
  private readonly blur: Program;
  private readonly blend: Program;
  private readonly frameTex: WebGLTexture;
  private readonly maskTex: WebGLTexture;
  private readonly pictureTex: WebGLTexture;
  private readonly modelTarget: Target;
  private readonly blurA: Target;
  private readonly blurB: Target;
  /** What the picture texture holds: a mode and the frame size it was drawn
   *  for (a built-in is drawn at the frame's size). */
  private pictureFor: { mode: StageMode; width: number; height: number } | null = null;
  private pictureRect: [number, number, number, number] = [0, 0, 1, 1];
  private lost = false;
  /** The frame size the viewport below was worked out for. */
  private viewFor: { width: number; height: number; view: ModelViewport } | null = null;

  constructor(
    private readonly canvas: Canvas,
    private readonly rules: CompositeRules,
    preserveDrawingBuffer = false,
  ) {
    const gl = canvas.getContext("webgl2", {
      alpha: false,
      antialias: false,
      depth: false,
      stencil: false,
      premultipliedAlpha: false,
      preserveDrawingBuffer,
      desynchronized: false,
      powerPreference: "low-power",
    }) as WebGL2RenderingContext | null;
    if (gl === null) throw new Error("no WebGL2 context");
    this.gl = gl;
    canvas.addEventListener("webglcontextlost", () => {
      this.lost = true;
    });
    this.copy = this.link(COPY, ["uTex", "uFlip"]);
    this.blur = this.link(BLUR, ["uTex", "uFlip", "uStep", "uSigma", "uRadius"]);
    this.blend = this.link(BLEND, ["uFrame", "uBack", "uMask", "uBackRect", "uMaskRect", "uFlip"]);
    this.frameTex = this.texture();
    this.maskTex = this.texture();
    this.pictureTex = this.texture();
    this.modelTarget = this.target(SEG_INPUT_WIDTH, SEG_INPUT_HEIGHT);
    this.blurA = this.target(1, 1);
    this.blurB = this.target(1, 1);
    // Rows of the mask are one byte wide.
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    gl.pixelStorei(gl.PACK_ALIGNMENT, 1);
  }

  writeModelInput(frame: CameraFrame, out: Uint8Array): void {
    this.check();
    const gl = this.gl;
    this.uploadFrame(frame);
    const view = this.viewOf(frame.width, frame.height);
    gl.bindFramebuffer(gl.FRAMEBUFFER, this.modelTarget.framebuffer);
    // Black bars around a frame whose shape is not the input's.
    gl.viewport(0, 0, SEG_INPUT_WIDTH, SEG_INPUT_HEIGHT);
    gl.clearColor(0, 0, 0, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    // The framebuffer's row 0 is the model's top row (`flip = 1`), so the
    // viewport takes the top-left rectangle as it is.
    gl.viewport(view.x, view.y, view.width, view.height);
    this.useCopy(this.frameTex, 1);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    gl.readPixels(0, 0, SEG_INPUT_WIDTH, SEG_INPUT_HEIGHT, gl.RGBA, gl.UNSIGNED_BYTE, out);
    this.check();
  }

  composite(frame: CameraFrame, mask: Uint8Array, mode: StageMode): void {
    this.check();
    const gl = this.gl;
    const { width, height } = frame;
    // `writeModelInput` uploaded this frame when the model ran on it; frames
    // between runs are uploaded here.
    this.uploadFrame(frame);
    gl.bindTexture(gl.TEXTURE_2D, this.maskTex);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.R8, SEG_INPUT_WIDTH, SEG_INPUT_HEIGHT, 0, gl.RED, gl.UNSIGNED_BYTE, mask);

    let back: WebGLTexture;
    let rect: [number, number, number, number];
    if (mode.kind === "blur") {
      const p = this.rules.blur(mode.strong, height);
      const divisor = Math.max(1, p.divisor);
      const w = Math.max(1, Math.ceil(width / divisor));
      const h = Math.max(1, Math.ceil(height / divisor));
      this.resize(this.blurA, w, h);
      this.resize(this.blurB, w, h);
      const radius = Math.min(MAX_BLUR_RADIUS, Math.max(1, Math.round(p.radius)));
      const sigma = Math.max(0.5, p.sigma);
      // Down-scale.
      gl.bindFramebuffer(gl.FRAMEBUFFER, this.blurA.framebuffer);
      gl.viewport(0, 0, w, h);
      this.useCopy(this.frameTex, 1);
      gl.drawArrays(gl.TRIANGLES, 0, 3);
      // Horizontal, then vertical.
      this.blurPass(this.blurA, this.blurB, 1 / w, 0, sigma, radius);
      this.blurPass(this.blurB, this.blurA, 0, 1 / h, sigma, radius);
      back = this.blurA.texture;
      rect = [0, 0, 1, 1];
    } else {
      this.preparePicture(mode, width, height);
      back = this.pictureTex;
      rect = this.pictureRect;
    }

    // The canvas carries the frame's own size, so the picture sent is as big
    // as the camera's.
    if (this.canvas.width !== width) this.canvas.width = width;
    if (this.canvas.height !== height) this.canvas.height = height;
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.viewport(0, 0, width, height);
    gl.useProgram(this.blend.program);
    this.bindUnit(0, this.frameTex);
    this.bindUnit(1, back);
    this.bindUnit(2, this.maskTex);
    gl.uniform1i(this.u(this.blend, "uFrame"), 0);
    gl.uniform1i(this.u(this.blend, "uBack"), 1);
    gl.uniform1i(this.u(this.blend, "uMask"), 2);
    gl.uniform4f(this.u(this.blend, "uBackRect"), rect[0], rect[1], rect[2], rect[3]);
    const m = maskUvRect(this.viewOf(width, height));
    gl.uniform4f(this.u(this.blend, "uMaskRect"), m[0], m[1], m[2], m[3]);
    gl.uniform1f(this.u(this.blend, "uFlip"), -1);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    this.check();
  }

  /** Frees the GPU objects. */
  dispose(): void {
    const gl = this.gl;
    for (const t of [this.frameTex, this.maskTex, this.pictureTex]) gl.deleteTexture(t);
    for (const t of [this.modelTarget, this.blurA, this.blurB]) {
      gl.deleteFramebuffer(t.framebuffer);
      gl.deleteTexture(t.texture);
    }
    for (const p of [this.copy, this.blur, this.blend]) gl.deleteProgram(p.program);
    const lose = gl.getExtension("WEBGL_lose_context");
    lose?.loseContext();
  }

  // -------------------------------------------------------------------------

  private check(): void {
    if (this.lost || this.gl.isContextLost()) throw new Error("WebGL context lost");
  }

  /** Where a `width` x `height` frame sits in the model input, whole pixels;
   *  worked out once per frame size. */
  private viewOf(width: number, height: number): ModelViewport {
    const held = this.viewFor;
    if (held !== null && held.width === width && held.height === height) return held.view;
    const view = modelViewport(this.rules.modelInputRect(width, height));
    this.viewFor = { width, height, view };
    return view;
  }

  private uploadFrame(frame: CameraFrame): void {
    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, this.frameTex);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, frame.source);
  }

  private preparePicture(mode: Exclude<StageMode, { kind: "blur" }>, width: number, height: number): void {
    const held = this.pictureFor;
    const sameMode = held !== null && held.mode === mode;
    // A photo is uploaded once per mode; a built-in is drawn at the frame's
    // own size, so again when the size changes.
    if (sameMode && (mode.kind === "picture" || (held.width === width && held.height === height))) return;
    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, this.pictureTex);
    if (mode.kind === "builtin") {
      const pixels = this.rules.builtin(mode.index, width, height);
      if (pixels === null) throw new Error("built-in background could not be drawn");
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, width, height, 0, gl.RGBA, gl.UNSIGNED_BYTE, pixels);
      this.pictureRect = [0, 0, 1, 1];
    } else {
      const bitmap = mode.bitmap;
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, bitmap);
      const [x, y, w, h] = this.rules.coverRect(bitmap.width, bitmap.height, width, height);
      const bw = Math.max(1, bitmap.width);
      const bh = Math.max(1, bitmap.height);
      this.pictureRect = [x / bw, y / bh, w / bw, h / bh];
    }
    this.pictureFor = { mode, width, height };
  }

  private blurPass(from: Target, to: Target, dx: number, dy: number, sigma: number, radius: number): void {
    const gl = this.gl;
    gl.bindFramebuffer(gl.FRAMEBUFFER, to.framebuffer);
    gl.viewport(0, 0, to.width, to.height);
    gl.useProgram(this.blur.program);
    this.bindUnit(0, from.texture);
    gl.uniform1i(this.u(this.blur, "uTex"), 0);
    gl.uniform1f(this.u(this.blur, "uFlip"), 1);
    gl.uniform2f(this.u(this.blur, "uStep"), dx, dy);
    gl.uniform1f(this.u(this.blur, "uSigma"), sigma);
    gl.uniform1i(this.u(this.blur, "uRadius"), radius);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }

  private useCopy(texture: WebGLTexture, flip: number): void {
    const gl = this.gl;
    gl.useProgram(this.copy.program);
    this.bindUnit(0, texture);
    gl.uniform1i(this.u(this.copy, "uTex"), 0);
    gl.uniform1f(this.u(this.copy, "uFlip"), flip);
  }

  private bindUnit(unit: number, texture: WebGLTexture): void {
    const gl = this.gl;
    gl.activeTexture(gl.TEXTURE0 + unit);
    gl.bindTexture(gl.TEXTURE_2D, texture);
  }

  private u(p: Program, name: string): WebGLUniformLocation | null {
    return p.uniforms.get(name) ?? null;
  }

  private texture(): WebGLTexture {
    const gl = this.gl;
    const t = gl.createTexture();
    if (t === null) throw new Error("createTexture failed");
    gl.bindTexture(gl.TEXTURE_2D, t);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    return t;
  }

  private target(width: number, height: number): Target {
    const gl = this.gl;
    const texture = this.texture();
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, width, height, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
    const framebuffer = gl.createFramebuffer();
    if (framebuffer === null) throw new Error("createFramebuffer failed");
    gl.bindFramebuffer(gl.FRAMEBUFFER, framebuffer);
    gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, texture, 0);
    if (gl.checkFramebufferStatus(gl.FRAMEBUFFER) !== gl.FRAMEBUFFER_COMPLETE) {
      throw new Error("framebuffer incomplete");
    }
    return { texture, framebuffer, width, height };
  }

  private resize(target: Target, width: number, height: number): void {
    if (target.width === width && target.height === height) return;
    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, target.texture);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, width, height, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
    target.width = width;
    target.height = height;
  }

  private link(fragment: string, uniforms: readonly string[]): Program {
    const gl = this.gl;
    const compile = (type: number, source: string): WebGLShader => {
      const s = gl.createShader(type);
      if (s === null) throw new Error("createShader failed");
      gl.shaderSource(s, source);
      gl.compileShader(s);
      if (gl.getShaderParameter(s, gl.COMPILE_STATUS) !== true) {
        const why = gl.getShaderInfoLog(s) ?? "";
        gl.deleteShader(s);
        throw new Error(`shader did not compile: ${why}`);
      }
      return s;
    };
    const vs = compile(gl.VERTEX_SHADER, VERTEX);
    const fs = compile(gl.FRAGMENT_SHADER, fragment);
    const program = gl.createProgram();
    if (program === null) throw new Error("createProgram failed");
    gl.attachShader(program, vs);
    gl.attachShader(program, fs);
    gl.linkProgram(program);
    gl.deleteShader(vs);
    gl.deleteShader(fs);
    if (gl.getProgramParameter(program, gl.LINK_STATUS) !== true) {
      throw new Error(`program did not link: ${gl.getProgramInfoLog(program) ?? ""}`);
    }
    const map = new Map<string, WebGLUniformLocation | null>();
    for (const name of uniforms) map.set(name, gl.getUniformLocation(program, name));
    return { program, uniforms: map };
  }
}
