// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Whether this browser can run a camera background at all, and which way
 * (ADR-0089 section 4, UX `docs/ux/call-backgrounds.md` section 3: a device
 * that cannot has no `Background` section rather than a picker that does
 * nothing).
 *
 * Needed: WebAssembly with SIMD (the module is built with `+simd128`), WebGL2,
 * and one way to make a track from processed frames:
 *
 * - `frames`: `MediaStreamTrackProcessor` + `MediaStreamTrackGenerator` on the
 *   window, `VideoFrame` and `OffscreenCanvas` (the frames are processed in a
 *   worker; Chrome and Edge);
 * - `canvas`: a `<video>` of the camera drawn into a WebGL2 canvas published
 *   with `captureStream`, on the main thread. Only the processed picture is
 *   ever drawn onto that canvas (the camera frame goes to textures), so
 *   whatever the browser captures from it is processed or blank.
 *
 * Seen 2026-11-08 in Playwright's builds (headless): Chromium takes `frames`;
 * Firefox 155 and WebKit (Safari 26.6) take `canvas` (neither has the frame
 * streams; Firefox's canvas track has no `requestFrame`). Not seen in the
 * shipped Chrome, Edge, Firefox or Safari, nor in the desktop app's
 * WKWebView, and nothing about a hidden tab (open question 3 of the ADR).
 */

export type BackgroundPath = "frames" | "canvas";

/** The SIMD probe of `wasm-feature-detect`: a module whose one function
 *  uses `i8x16.popcnt`. Valid only where wasm SIMD is. */
const SIMD_PROBE = new Uint8Array([
  0, 97, 115, 109, 1, 0, 0, 0, 1, 5, 1, 96, 0, 1, 123, 3, 2, 1, 0, 10, 10, 1, 8, 0, 65, 0, 253, 15, 253, 98, 11,
]);

/** What the browser offers, injectable for tests. */
export interface SupportProbe {
  readonly wasmSimd: boolean;
  readonly webgl2: boolean;
  readonly frameStreams: boolean;
  readonly canvasCapture: boolean;
}

/** The path a browser with `probe` takes, or `null` when it can run none. */
export function backgroundPath(probe: SupportProbe): BackgroundPath | null {
  if (!probe.wasmSimd || !probe.webgl2) return null;
  if (probe.frameStreams) return "frames";
  if (probe.canvasCapture) return "canvas";
  return null;
}

function hasWasmSimd(): boolean {
  try {
    return typeof WebAssembly === "object" && WebAssembly.validate(SIMD_PROBE);
  } catch {
    return false;
  }
}

function hasWebgl2(): boolean {
  try {
    if (typeof document === "undefined") return false;
    const canvas = document.createElement("canvas");
    const gl = canvas.getContext("webgl2");
    if (gl === null) return false;
    gl.getExtension("WEBGL_lose_context")?.loseContext();
    return true;
  } catch {
    return false;
  }
}

function hasFrameStreams(): boolean {
  const g = globalThis as Record<string, unknown>;
  return (
    typeof g["MediaStreamTrackProcessor"] === "function" &&
    typeof g["MediaStreamTrackGenerator"] === "function" &&
    typeof g["VideoFrame"] === "function" &&
    typeof g["OffscreenCanvas"] === "function" &&
    typeof Worker === "function"
  );
}

function hasCanvasCapture(): boolean {
  try {
    if (typeof document === "undefined") return false;
    const canvas = document.createElement("canvas") as HTMLCanvasElement & {
      captureStream?: (rate?: number) => MediaStream;
    };
    return typeof canvas.captureStream === "function";
  } catch {
    return false;
  }
}

let cached: BackgroundPath | null | undefined;

/** This browser's path, probed once per page. */
export function detectBackgroundPath(): BackgroundPath | null {
  if (cached === undefined) {
    cached = backgroundPath({
      wasmSimd: hasWasmSimd(),
      webgl2: hasWebgl2(),
      frameStreams: hasFrameStreams(),
      canvasCapture: hasCanvasCapture(),
    });
  }
  return cached;
}

/** Whether the `Background` section is offered at all. */
export function backgroundSupported(): boolean {
  return detectBackgroundPath() !== null;
}
