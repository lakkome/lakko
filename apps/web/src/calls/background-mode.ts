// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What a running background draws behind the person (ADR-0089), once the
 * person's choice is resolved: a blur, a built-in picture (by its index in
 * the shared strip order, drawn by `lakko_segment`), or a decoded picture
 * (an own photo). Crosses to the background worker by structured clone.
 */
export type StageMode =
  | { readonly kind: "blur"; readonly strong: boolean }
  | { readonly kind: "builtin"; readonly index: number }
  | { readonly kind: "picture"; readonly bitmap: ImageBitmap };

/** One camera frame as the compositor reads it. */
export interface CameraFrame {
  readonly source: TexImageSource;
  readonly width: number;
  readonly height: number;
}
